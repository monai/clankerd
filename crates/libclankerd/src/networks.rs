//! Persistent virtual networks; one process-shared lock protects membership and allocation.
use crate::store::{LifecycleLock, Store};
use crate::{Engine, Error, HostConfig, NetworkMode, Result, SharedNetwork};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkKind {
    Shared,
    HostOnly,
}
impl std::str::FromStr for NetworkKind {
    type Err = String;
    fn from_str(text: &str) -> std::result::Result<Self, String> {
        match text {
            "shared" => Ok(Self::Shared),
            "host-only" => Ok(Self::HostOnly),
            _ => Err("virtual network mode must be shared or host-only".into()),
        }
    }
}
impl NetworkKind {
    fn mode(self) -> NetworkMode {
        match self {
            Self::Shared => NetworkMode::Shared,
            Self::HostOnly => NetworkMode::HostOnly,
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VirtualNetwork {
    pub id: String,
    pub name: String,
    pub mode: NetworkKind,
    pub subnet: SharedNetwork,
    #[serde(default)]
    pub ipv6_prefix: Option<std::net::Ipv6Addr>,
    pub created: SystemTime,
}

impl VirtualNetwork {
    /// Select this resource's membership and addressing, preserving unrelated host settings.
    /// Persist the result with [`crate::Machine::update`] before starting the machine.
    pub fn configure_member(&self, host: &mut HostConfig) {
        host.virtual_network = Some(self.name.clone());
        host.network_mode = self.mode.mode();
        host.shared_network = self.subnet.clone();
        host.shared_ipv6_prefix = self.ipv6_prefix;
        host.shared_peer_participation = true;
        host.bridge_interface = None;
    }
}

pub(crate) struct Networks<'a> {
    store: &'a Store,
    root: PathBuf,
    _lock: LifecycleLock,
}
impl<'a> Networks<'a> {
    pub(crate) fn lock(store: &'a Store) -> Result<Self> {
        let root = store.networks_dir();
        fs::create_dir_all(&root)?;
        let lock = LifecycleLock::acquire(&root, true)?;
        Ok(Self {
            store,
            root,
            _lock: lock,
        })
    }
    fn path(&self, name: &str) -> Result<PathBuf> {
        resource_path(&self.root, name)
    }
    fn get(&self, name: &str) -> Result<VirtualNetwork> {
        read_resource(&self.root, name)
    }

    fn list(&self) -> Result<Vec<VirtualNetwork>> {
        let mut networks = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let path = entry?.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                networks.push(serde_json::from_slice::<VirtualNetwork>(&fs::read(path)?)?);
            }
        }
        networks.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(networks)
    }
    fn occupied(&self) -> Result<Vec<SharedNetwork>> {
        let mut occupied: Vec<_> = self
            .list()?
            .into_iter()
            .map(|network| network.subnet)
            .collect();
        for id in self.store.ids()? {
            match self.store.load(&id) {
                Ok((record, _)) if record.host_config.network_mode != NetworkMode::Bridged => {
                    occupied.push(record.host_config.shared_network)
                }
                Ok(_) => {}
                Err(error) if error.kind() == crate::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(occupied)
    }
    fn create(
        &self,
        name: &str,
        mode: NetworkKind,
        subnet: Option<SharedNetwork>,
        allocate_ipv6: bool,
    ) -> Result<VirtualNetwork> {
        let path = self.path(name)?;
        if path.exists() {
            return Err(Error::conflict(format!(
                "virtual network already exists: {name}"
            )));
        }
        let occupied = self.occupied()?;
        let subnet = match subnet {
            Some(subnet) => {
                subnet.addresses()?;
                if occupied.iter().any(|other| overlaps(&subnet, other)) {
                    return Err(Error::conflict(
                        "virtual network subnet overlaps an existing network or machine",
                    ));
                }
                subnet
            }
            None => (64..=254)
                .filter(|third| !matches!(third, 105 | 128))
                .map(|third| SharedNetwork {
                    address: std::net::Ipv4Addr::new(192, 168, third, 0),
                    prefix_len: 24,
                })
                .find(|candidate| !occupied.iter().any(|other| overlaps(candidate, other)))
                .ok_or_else(|| Error::unavailable("no free automatic virtual network subnet"))?,
        };
        let id = crate::engine::random_id()?;
        let ipv6_prefix = if mode == NetworkKind::Shared && allocate_ipv6 {
            let entropy = u64::from_str_radix(&id[..14], 16)
                .map_err(|_| Error::unavailable("invalid network identity entropy"))?;
            let prefix = std::net::Ipv6Addr::from(u128::from((0xfd_u64 << 56) | entropy) << 64);
            if self
                .list()?
                .iter()
                .any(|network| network.ipv6_prefix == Some(prefix))
            {
                return Err(Error::conflict(
                    "generated IPv6 prefix is already allocated; retry network creation",
                ));
            }
            Some(prefix)
        } else {
            None
        };
        let network = VirtualNetwork {
            id,
            name: name.into(),
            mode,
            subnet,
            ipv6_prefix,
            created: SystemTime::now(),
        };
        crate::store::write_atomic(&path, &serde_json::to_vec_pretty(&network)?)?;
        Ok(network)
    }
    pub(crate) fn assign(&self, host: &mut HostConfig, machine_id: &str) -> Result<()> {
        if host.network_mode == NetworkMode::Bridged {
            if host.virtual_network.is_some() {
                return Err(Error::invalid_parameter(
                    "bridged attachment cannot join a virtual network",
                ));
            }
            return Ok(());
        }
        let network = if let Some(name) = &host.virtual_network {
            self.get(name)?
        } else if !host.shared_peer_participation {
            let name = format!("machine-{machine_id}");
            let kind = if host.network_mode == NetworkMode::HostOnly {
                NetworkKind::HostOnly
            } else {
                NetworkKind::Shared
            };
            match self.get(&name) {
                Ok(network) => {
                    let (record, _) = self.store.load(machine_id)?;
                    if network.mode != kind
                        || record.host_config.virtual_network.as_deref() != Some(&name)
                    {
                        return Err(Error::conflict(
                            "singleton virtual network conflicts with existing membership",
                        ));
                    }
                    network
                }
                Err(error) if error.kind() == crate::ErrorKind::NotFound => {
                    self.create(&name, kind, None, true)?
                }
                Err(error) => return Err(error),
            }
        } else {
            let kind = if host.network_mode == NetworkMode::HostOnly {
                NetworkKind::HostOnly
            } else {
                NetworkKind::Shared
            };
            let name = format!(
                "{}-{}-{}",
                if kind == NetworkKind::Shared {
                    "shared"
                } else {
                    "host-only"
                },
                host.shared_network.address,
                host.shared_network.prefix_len
            );
            match self.get(&name) {
                Ok(network) => {
                    if network.mode != kind || network.subnet != host.shared_network {
                        return Err(Error::conflict(
                            "implicit virtual network name conflicts with requested mode or subnet",
                        ));
                    }
                    network
                }
                Err(error) if error.kind() == crate::ErrorKind::NotFound => {
                    self.create(&name, kind, Some(host.shared_network.clone()), false)?
                }
                Err(error) => return Err(error),
            }
        };
        network.configure_member(host);
        Ok(())
    }
}
pub(crate) fn resource_id(store: &Store, name: &str) -> Result<String> {
    Ok(read_resource(&store.networks_dir(), name)?.id)
}
fn resource_path(root: &Path, name: &str) -> Result<PathBuf> {
    if !crate::volumes::valid_name(name) || name.len() > 128 {
        return Err(Error::invalid_parameter("invalid virtual network name"));
    }
    Ok(root.join(format!("{name}.json")))
}
fn read_resource(root: &Path, name: &str) -> Result<VirtualNetwork> {
    match fs::read(resource_path(root, name)?) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Err(Error::not_found(format!("no such virtual network: {name}")))
        }
        Err(error) => Err(error.into()),
    }
}

fn overlaps(a: &SharedNetwork, b: &SharedNetwork) -> bool {
    let prefix = a.prefix_len.min(b.prefix_len);
    let mask = u32::MAX << (32 - prefix);
    u32::from(a.address) & mask == u32::from(b.address) & mask
}
impl Engine {
    pub fn create_network(
        &self,
        name: &str,
        mode: NetworkKind,
        subnet: Option<SharedNetwork>,
    ) -> Result<VirtualNetwork> {
        Networks::lock(&self.inner.store)?.create(name, mode, subnet, true)
    }
    pub fn networks(&self) -> Result<Vec<VirtualNetwork>> {
        Networks::lock(&self.inner.store)?.list()
    }
    pub fn inspect_network(&self, name: &str) -> Result<VirtualNetwork> {
        Networks::lock(&self.inner.store)?.get(name)
    }
    pub fn remove_network(&self, name: &str) -> Result<()> {
        let networks = Networks::lock(&self.inner.store)?;
        networks.get(name)?;
        for id in self.inner.store.ids()? {
            let (record, _) = self.inner.store.load(&id)?;
            if record.host_config.virtual_network.as_deref() == Some(name) {
                return Err(Error::conflict(format!(
                    "virtual network is in use by {}",
                    record.name
                )));
            }
        }
        fs::remove_file(networks.path(name)?)?;
        Ok(())
    }
}
