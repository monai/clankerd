//! The configuration file libclankerd hands to `clankerd-vmspawn`
//! (`clankerd-vmspawn --spec FILE`), plus the boot-directory conventions
//! shared by both sides.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// vsock port clankerd-guestd listens on for `io.clankerd.Guest`.
pub const GUEST_VSOCK_PORT: u32 = 1024;
/// vsock port on the host that guestd dials for tunnels (the library's
/// host-side tunnel server).
pub const HOST_VSOCK_PORT: u32 = 1025;

/// Files in the boot directory (the guest's root over virtio-fs).
pub const BOOT_GUESTD: &str = "clankerd-guestd";
pub const BOOT_WORKLOAD: &str = "workload.json";

/// Where the root disk shows up in the guest: the first virtio-blk device.
pub const ROOT_DEVICE: &str = "/dev/vda";
/// Where guestd keeps the boot directory inside the machine after it pivoted
/// into the root disk (it holds the static e2fsprogs binaries).
pub const GUEST_BOOT_MOUNT: &str = "/run/clankerd/boot";

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
    /// Unix socket where the library's tunnel server listens; guest
    /// connections to [`HOST_VSOCK_PORT`] are forwarded there.
    pub host_socket: PathBuf,
    pub cpus: u8,
    pub memory_mib: u32,
    /// Raw ext4 image attached as the guest's first block device
    /// ([`ROOT_DEVICE`]). With `populate` it is the blank disk to fill;
    /// otherwise guestd mounts it and pivots into it.
    #[serde(default)]
    pub root_disk: Option<PathBuf>,
    /// Population boot: guestd runs without a workload and serves only the
    /// root-disk methods, then powers off.
    #[serde(default)]
    pub populate: bool,
}
