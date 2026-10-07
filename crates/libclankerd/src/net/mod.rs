//! The `NetBackend` extension point: whatever gives a machine's virtio NIC
//! its network (gvproxy now, vmnet in v2).
//!
//! A backend runs one sidecar per machine. It is started before the VMM, ends
//! with the machine, and must not die with the library process (machines
//! outlive it), so backends keep their own record of the sidecar in the
//! machine directory and [`NetBackend::stop`] works from that record alone.

mod fetch;
mod gvproxy;

use std::path::PathBuf;

use clankerd_proto::guest::BlockedEndpoint;

use crate::error::Result;

pub use fetch::{GVPROXY_SHA256, GVPROXY_URL, GVPROXY_VERSION, GvproxyFetcher};
pub use gvproxy::GvproxyBackend;

/// What a backend needs to run one machine's sidecar.
#[derive(Debug, Clone)]
pub struct NetSpec {
    pub machine_id: String,
    /// The machine's state directory (pid file, sidecar log).
    pub dir: PathBuf,
    /// Where the sidecar must listen for the VMM's NIC (short path: unix
    /// socket paths are length-limited).
    pub socket: PathBuf,
}

/// How the VMM attaches the NIC, and what the guest must be told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetAttachment {
    /// The sidecar's unix datagram socket; it exists when `start` returns.
    pub socket: PathBuf,
    pub mac: [u8; 6],
    /// virtio-net feature bits to offer.
    pub features: u32,
    /// Send the vfkit magic when connecting to `socket`.
    pub vfkit: bool,
    /// Destinations inside the virtual network the guest must not reach.
    pub blocked: Vec<BlockedEndpoint>,
}

pub trait NetBackend: Send + Sync {
    /// Starts the machine's sidecar and returns once the VMM can attach.
    fn start(&self, spec: &NetSpec) -> Result<NetAttachment>;

    /// Ends the machine's sidecar. Idempotent; safe for a sidecar started by
    /// an earlier process and for one that already died.
    fn stop(&self, spec: &NetSpec);
}
