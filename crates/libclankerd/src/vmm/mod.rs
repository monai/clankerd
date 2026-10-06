//! The `Vmm` extension point: whatever boots a machine and runs clankerd-guestd in it.
//!
//! libkrun (via clankerd-vmspawn) is the production implementation (later ticket);
//! [`LocalProcessVmm`] runs guestd as a local process and is the stand-in used
//! by seam A tests and development.

mod process;

pub use process::LocalProcessVmm;

use std::path::PathBuf;

use clankerd_proto::guest::Workload;

use crate::error::Result;

/// Everything a VMM needs to boot one machine.
#[derive(Debug, Clone)]
pub struct BootSpec {
    pub machine_id: String,
    /// The machine's private state directory (VMM may keep files here).
    pub dir: PathBuf,
    /// Unix socket path the VMM must make reachable as the guest's varlink
    /// endpoint (stands in for vsock).
    pub guest_socket: PathBuf,
    /// Unix socket path the VMM must make reachable to the guest as the host's
    /// tunnel endpoint (stands in for a vsock port the guest dials).
    pub host_socket: PathBuf,
    /// File the VMM must create with the workload's exit code once it ended,
    /// so the result survives the death of the library process.
    pub exit_file: PathBuf,
    pub workload: Workload,
    pub cpus: Option<u32>,
    pub memory: Option<u64>,
}

/// A booted machine. The VMM process must outlive the library process.
#[derive(Debug, Clone, Copy)]
pub struct BootHandle {
    /// Process to signal to force the machine down.
    pub pid: u32,
}

pub trait Vmm: Send + Sync {
    /// Starts the machine and returns without waiting for guest readiness.
    fn boot(&self, spec: &BootSpec) -> Result<BootHandle>;
}

/// Used when no VMM is configured: every boot fails with a clear error.
#[derive(Debug, Default)]
pub struct UnavailableVmm;

impl Vmm for UnavailableVmm {
    fn boot(&self, _spec: &BootSpec) -> Result<BootHandle> {
        Err(crate::Error::unavailable(
            "no VMM backend configured (libkrun support is not wired up yet)",
        ))
    }
}
