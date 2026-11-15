use super::NetSpec;
use crate::{Error, NetworkMode, Result};
use clankerd_network_ipc::{Connection, Request, Response, Settings};
use clankerd_proto::spawn::NetDevice;
use std::{fs, path::Path, sync::Arc};

pub trait NetworkService: Send + Sync {
    fn request(&self, request: &Request) -> std::io::Result<Response>;
}
struct SystemService;
impl NetworkService for SystemService {
    fn request(&self, request: &Request) -> std::io::Result<Response> {
        Connection::connect()?
            .request(request)
            .map(|reply| reply.response)
    }
}
/// Controls native interfaces owned by the system network helper.
pub struct Network {
    service: Arc<dyn NetworkService>,
}
impl Default for Network {
    fn default() -> Self {
        Self {
            service: Arc::new(SystemService),
        }
    }
}
impl Network {
    pub fn with_service(service: Arc<dyn NetworkService>) -> Self {
        Self { service }
    }
    fn request(&self, request: &Request) -> Result<Response> {
        self.service
            .request(request)
            .map_err(|e| Error::unavailable(e.to_string()))
    }
}
const LEASE: &str = "net-lease.json";
/// A packet descriptor and the XPC connection that owns its native interface.
pub struct Attachment {
    pub packet: std::os::fd::OwnedFd,
    pub connection: Connection,
}
impl Attachment {
    pub fn open(device: &NetDevice) -> std::io::Result<Self> {
        let connection = Connection::connect()?;
        let reply = connection.request(&Request::Attach {
            key: device.key.clone(),
            generation: device.generation.clone(),
        })?;
        Ok(Self {
            packet: reply
                .packet
                .ok_or_else(|| std::io::Error::other("daemon returned no packet socket"))?,
            connection,
        })
    }
}
fn lease(dir: &Path) -> Option<NetDevice> {
    serde_json::from_slice(&fs::read(dir.join(LEASE)).ok()?).ok()
}
impl Network {
    pub(crate) fn start(&self, spec: &NetSpec) -> Result<NetDevice> {
        crate::config::validate_bridge_config(&spec.host_config)?;
        super::validate_host_config(&spec.host_config)?;
        let host = &spec.host_config;
        if host.network_mode == NetworkMode::Shared && host.shared_ipv6_prefix.is_none() {
            return Err(Error::invalid_parameter(
                "shared attachment requires its virtual network's persisted IPv6 prefix",
            ));
        }
        if host.network_mode == NetworkMode::Bridged {
            let interface = host.bridge_interface.as_deref().unwrap();
            let supported = self.request(&Request::List {})?.interfaces;
            super::bridge::validate_interface(interface, &supported)?;
        }
        self.stop(spec)?;
        let key = spec.dir.to_string_lossy().into_owned();
        let settings = settings(spec)?;
        let response = self.request(&Request::Create {
            key: key.clone(),
            settings,
        })?;
        let generation = response
            .generation
            .ok_or_else(|| Error::unavailable("daemon returned no interface generation"))?;
        let device = NetDevice {
            key: key.clone(),
            generation: generation.clone(),
            mac: machine_mac(&spec.machine_id),
        };
        let persisted = (|| -> Result<()> {
            if !response.connected
                || response
                    .prefix
                    .as_deref()
                    .map(str::parse::<std::net::Ipv6Addr>)
                    .transpose()
                    .map_err(|e| Error::unavailable(e.to_string()))?
                    != host.shared_ipv6_prefix
            {
                return Err(Error::unavailable(
                    "daemon did not acknowledge the requested network prefix",
                ));
            }
            let temporary = spec.dir.join("net-lease.tmp");
            fs::write(&temporary, serde_json::to_vec(&device)?)?;
            fs::rename(temporary, spec.dir.join(LEASE))?;
            Ok(())
        })();
        if let Err(error) = persisted {
            let _ = self.request(&Request::Stop { key, generation });
            return Err(error);
        }
        Ok(device)
    }
    pub(crate) fn stop(&self, spec: &NetSpec) -> Result<()> {
        if let Some(lease) = lease(&spec.dir) {
            self.request(&Request::Stop {
                key: lease.key,
                generation: lease.generation,
            })?;
            match fs::remove_file(spec.dir.join(LEASE)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    pub(crate) fn connected(&self, spec: &NetSpec) -> bool {
        lease(&spec.dir).is_some_and(|lease| {
            self.request(&Request::Status {
                key: lease.key,
                generation: lease.generation,
            })
            .is_ok_and(|response| response.connected)
        })
    }
}
fn settings(spec: &NetSpec) -> Result<Settings> {
    let host = &spec.host_config;
    let bridged = host.network_mode == NetworkMode::Bridged;
    let (start, end, mask) = host.shared_network.addresses()?;
    Ok(Settings {
        interface_id: interface_identity(&format!(
            "{}:{}",
            unsafe { libc::getuid() },
            spec.machine_id
        )),
        operation_mode: match host.network_mode {
            NetworkMode::Shared => "shared",
            NetworkMode::HostOnly => "host",
            NetworkMode::Bridged => "bridged",
        }
        .into(),
        start_address: (!bridged).then(|| start.to_string()),
        end_address: (!bridged).then(|| end.to_string()),
        subnet_mask: (!bridged).then(|| mask.to_string()),
        nat66_prefix: host.shared_ipv6_prefix.map(|p| p.to_string()),
        shared_interface: host.bridge_interface.clone(),
        network_id: spec.network_id.clone(),
        subnet: format!(
            "{}/{}",
            host.shared_network.address, host.shared_network.prefix_len
        ),
        firewall: !bridged && std::env::var("CLANKERD_NETWORK_PF").as_deref() != Ok("off"),
    })
}
fn identity_bytes(id: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(id.as_bytes()).into()
}
fn machine_mac(id: &str) -> [u8; 6] {
    let hash = identity_bytes(id);
    [0x02, hash[1], hash[2], hash[3], hash[4], hash[5]]
}
fn interface_identity(id: &str) -> String {
    let h = identity_bytes(id);
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:01x}{:02x}-8{:01x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        h[0],
        h[1],
        h[2],
        h[3],
        h[4],
        h[5],
        h[6] & 15,
        h[7],
        h[8] & 15,
        h[9],
        h[10],
        h[11],
        h[12],
        h[13],
        h[14],
        h[15]
    )
}
