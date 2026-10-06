//! The configuration file libclankerd hands to `clankerd-vmspawn`
//! (`clankerd-vmspawn --spec FILE`), plus the boot-directory conventions
//! shared by both sides.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// vsock port clankerd-guestd listens on for `io.clankerd.Guest`.
pub const GUEST_VSOCK_PORT: u32 = 1024;

/// Files in the boot directory (the guest's root over virtio-fs).
pub const BOOT_GUESTD: &str = "clankerd-guestd";
pub const BOOT_WORKLOAD: &str = "workload.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpawnSpec {
    /// Host directory served as the guest root; holds guestd and its workload.
    pub boot_dir: PathBuf,
    /// Receives kernel, init and guestd console output.
    pub console_log: PathBuf,
    /// Written with the workload's exit code once guestd reports it.
    pub exit_file: PathBuf,
    /// Unix socket that reaches guestd's vsock port. The helper makes it
    /// available (libkrun creates it); the helper's own listener, inherited
    /// via `LISTEN_FDS`, proxies to it.
    pub vsock_socket: PathBuf,
    pub cpus: u8,
    pub memory_mib: u32,
}
