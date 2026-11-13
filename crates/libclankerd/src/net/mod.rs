//! The `NetBackend` extension point: whatever gives a machine's virtio NIC
//! its network through the clankerd-owned native attachment.
//!
mod bridge;
mod egress;
pub mod relay;
mod vmnet;

use std::path::PathBuf;

use crate::error::Result;

pub use vmnet::{NetworkService, VmnetBackend};

/// What a backend needs to run one machine's interface.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetSpec {
    pub machine_id: String,
    /// Stable resource identity, independent of names, state paths and native addressing.
    pub network_id: Option<String>,
    pub host_config: crate::HostConfig,
    /// The machine's state directory (interface lease).
    pub dir: PathBuf,
    /// Stable attachment identity; the VMM creates its relay socket beside this path.
    pub socket: PathBuf,
}

/// How the VMM attaches the NIC, and what the guest must be told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetAttachment {
    /// Attachment identity used by the VMM to obtain its packet socket.
    pub socket: PathBuf,
    pub mac: [u8; 6],
    /// virtio-net feature bits to offer.
    pub features: u32,
    /// Send the vfkit magic when connecting to `socket`.
    pub vfkit: bool,
}

pub trait NetBackend: Send + Sync {
    /// Starts the machine's interface and returns once the VMM can attach.
    fn start(&self, spec: &NetSpec) -> Result<NetAttachment>;

    /// Ends the machine's interface. Idempotent; safe for an interface started by
    /// an earlier process and for one that already died.
    fn stop(&self, spec: &NetSpec);

    /// Current native attachment health, independent of guest management.
    fn connected(&self, _spec: &NetSpec) -> bool {
        true
    }
}

pub(crate) fn validate_host_config(host: &crate::HostConfig) -> Result<()> {
    host.shared_network.addresses()?;
    if let Some(prefix) = host.shared_ipv6_prefix
        && (host.network_mode != crate::NetworkMode::Shared
            || prefix.octets()[0] != 0xfd
            || u128::from(prefix) & u128::from(u64::MAX) != 0)
    {
        return Err(crate::Error::invalid_parameter(
            "native IPv6 prefix requires shared mode and an aligned fd00::/8 ULA /64",
        ));
    }
    Ok(())
}
