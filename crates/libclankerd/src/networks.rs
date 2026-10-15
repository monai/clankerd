//! Persistent virtual networks; one process-shared lock protects membership and allocation.
use crate::store::{LifecycleLock, Store};
use crate::{Engine, Error, HostConfig, NetworkMode, Result, SharedNetwork};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
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
    pub created: SystemTime,
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
        if !crate::volumes::valid_name(name) || name.len() > 128 {
            return Err(Error::invalid_parameter("invalid virtual network name"));
        }
        Ok(self.root.join(format!("{name}.json")))
    }
    fn get(&self, name: &str) -> Result<VirtualNetwork> {
        let path = self.path(name)?;
        match fs::read(path) {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(Error::not_found(format!("no such virtual network: {name}")))
            }
            Err(error) => Err(error.into()),
        }
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
        let network = VirtualNetwork {
            id: crate::engine::random_id()?,
            name: name.into(),
            mode,
            subnet,
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
            self.create(
                &format!("machine-{machine_id}"),
                if host.network_mode == NetworkMode::HostOnly {
                    NetworkKind::HostOnly
                } else {
                    NetworkKind::Shared
                },
                None,
            )?
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
                Ok(network) => network,
                Err(error) if error.kind() == crate::ErrorKind::NotFound => {
                    self.create(&name, kind, Some(host.shared_network.clone()))?
                }
                Err(error) => return Err(error),
            }
        };
        host.virtual_network = Some(network.name);
        host.network_mode = network.mode.mode();
        host.shared_network = network.subnet;
        // Compatibility field now describes native network participation, never a per-member veto.
        host.shared_peer_participation = true;
        Ok(())
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
        Networks::lock(&self.inner.store)?.create(name, mode, subnet)
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
