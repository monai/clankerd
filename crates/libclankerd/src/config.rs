//! Machine configuration, split like Docker's `Config` / `HostConfig`.

use std::net::IpAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::mount::Mount;

/// What runs: the image and how its main process is invoked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineConfig {
    pub image: String,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    /// `KEY=value` entries.
    pub env: Vec<String>,
    pub user: String,
    pub working_dir: String,
    pub tty: bool,
    pub open_stdin: bool,
}

impl MachineConfig {
    /// Names of the fields that differ from `other`.
    pub(crate) fn differing_fields(&self, other: &MachineConfig) -> Vec<&'static str> {
        [
            ("image", self.image != other.image),
            ("entrypoint", self.entrypoint != other.entrypoint),
            ("cmd", self.cmd != other.cmd),
            ("env", self.env != other.env),
            ("user", self.user != other.user),
            ("working_dir", self.working_dir != other.working_dir),
            ("tty", self.tty != other.tty),
            ("open_stdin", self.open_stdin != other.open_stdin),
        ]
        .into_iter()
        .filter_map(|(name, differs)| differs.then_some(name))
        .collect()
    }
}

/// How the machine is hosted: resources, mounts, ports, policies.
/// Restart and pull policy arrive with later tickets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    pub cpus: Option<u32>,
    /// Bytes.
    pub memory: Option<u64>,
    /// Bind mounts and the machine's (at most one) named volume.
    pub mounts: Vec<Mount>,
    /// Guest ports published on host loopback, fixed for the machine's life.
    pub port_bindings: Vec<PortBinding>,
    /// Host loopback ports reachable at the same port on the guest's loopback.
    pub host_gateway_ports: Vec<u16>,
    /// Host unix sockets exposed at paths inside the guest.
    pub socket_bindings: Vec<SocketBinding>,
}

/// A guest TCP port published on the host. The host side is loopback only:
/// `host_ip` may name a loopback address but never anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    /// Loopback address to listen on; `None` means 127.0.0.1.
    #[serde(default)]
    pub host_ip: Option<IpAddr>,
    /// `0` picks a free port (see `PublishedPort::host_port`).
    pub host_port: u16,
    pub guest_port: u16,
}

impl PortBinding {
    /// `127.0.0.1:host_port` -> guest `guest_port`.
    pub fn loopback(host_port: u16, guest_port: u16) -> Self {
        PortBinding {
            host_ip: None,
            host_port,
            guest_port,
        }
    }
}

/// A host unix socket exposed inside the guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocketBinding {
    /// Where the guest sees the socket, e.g. `/var/run/clankerd.sock`.
    pub guest_path: PathBuf,
    /// The host socket it leads to.
    pub host_path: PathBuf,
}
