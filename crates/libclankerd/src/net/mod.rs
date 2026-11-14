//! Native interface lifecycle through the system network helper.
mod bridge;
mod vmnet;

use crate::Result;
use std::path::PathBuf;

pub use vmnet::{Attachment, Network, NetworkService};

/// Inputs for one native interface.
pub(crate) struct NetSpec {
    pub machine_id: String,
    /// Stable resource identity, independent of names, state paths and native addressing.
    pub network_id: Option<String>,
    pub host_config: crate::HostConfig,
    /// The machine's state directory (interface lease).
    pub dir: PathBuf,
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
