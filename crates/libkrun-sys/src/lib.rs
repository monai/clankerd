//! libkrun 1.19.x bindings and a single safe entry point, [`boot`].
//!
//! On macOS the crate links Homebrew's `libkrun` (see `build.rs`). Everywhere
//! else [`boot`] fails with [`ErrorKind::Unsupported`], so the workspace builds
//! and its tests run on Linux.
//!
//! libkrun's `krun_start_enter` takes over the calling process and exits it
//! when the guest powers off: call [`boot`] only from a dedicated helper
//! process (clankerd-vmspawn).

use std::fmt;
use std::path::PathBuf;

#[cfg(target_os = "macos")]
mod ffi;
#[cfg(target_os = "macos")]
mod hv;

/// Hypervisor.framework's `hv_return_t` values we tell apart.
const HV_NO_DEVICE: u32 = 0xfae9_4006;
const HV_DENIED: u32 = 0xfae9_4007;
const HV_UNSUPPORTED: u32 = 0xfae9_400f;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorKind {
    /// The process lacks `com.apple.security.hypervisor`.
    EntitlementMissing,
    /// Hypervisor.framework cannot create a VM here.
    HypervisorUnavailable,
    /// libkrunfw (the kernel library) cannot be loaded.
    KernelLibraryMissing,
    /// Any other failing libkrun call.
    Libkrun,
    /// Not running on macOS.
    Unsupported,
    /// The boot configuration cannot be passed to C (interior NUL).
    InvalidConfig,
}

#[derive(Debug, Clone)]
pub struct Error {
    kind: ErrorKind,
    message: String,
}

impl Error {
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// Interprets an `hv_return_t` from `hv_vm_create`.
    pub fn from_hv_return(code: u32) -> Self {
        let (kind, message) = match code {
            HV_DENIED => (
                ErrorKind::EntitlementMissing,
                "the hypervisor entitlement is missing: clankerd-vmspawn must be signed with \
                 com.apple.security.hypervisor (run `make rust-sign`, or \
                 `codesign --force -s - --entitlements crates/clankerd-vmspawn/entitlements.plist \
                 clankerd-vmspawn`)"
                    .to_owned(),
            ),
            HV_NO_DEVICE | HV_UNSUPPORTED => (
                ErrorKind::HypervisorUnavailable,
                format!(
                    "Hypervisor.framework cannot create a virtual machine on this Mac (code \
                     {code:#x}); it needs Apple Silicon and macOS 14 or newer, and cannot run \
                     inside another virtual machine without nested virtualization"
                ),
            ),
            _ => (
                ErrorKind::HypervisorUnavailable,
                format!("Hypervisor.framework failed to create a virtual machine (code {code:#x})"),
            ),
        };
        Error { kind, message }
    }

    /// Interprets a negative errno returned by a libkrun call.
    pub fn from_krun(call: &str, code: i32) -> Self {
        let errno = -code;
        if call == "krun_start_enter" && errno == libc_enoent() {
            return Error {
                kind: ErrorKind::KernelLibraryMissing,
                message: "libkrunfw (the guest kernel library) could not be loaded; \
                          install it with `brew install libkrunfw`; if already installed, \
                          check that DYLD_FALLBACK_LIBRARY_PATH includes /opt/homebrew/lib"
                    .to_owned(),
            };
        }
        Error {
            kind: ErrorKind::Libkrun,
            message: format!(
                "{call} failed: {}",
                std::io::Error::from_raw_os_error(errno)
            ),
        }
    }

    #[cfg(not(target_os = "macos"))]
    fn unsupported() -> Self {
        Error {
            kind: ErrorKind::Unsupported,
            message: "booting machines with libkrun is only supported on macOS".to_owned(),
        }
    }

    #[cfg(target_os = "macos")]
    fn invalid_config(what: &str) -> Self {
        Error {
            kind: ErrorKind::InvalidConfig,
            message: format!("{what} contains a NUL byte"),
        }
    }
}

const fn libc_enoent() -> i32 {
    2
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Error {}

/// A vsock port bridged to a unix socket on the host.
#[derive(Debug, Clone)]
pub struct VsockPort {
    pub port: u32,
    pub host_socket: PathBuf,
    /// `true`: libkrun creates the socket and forwards host connections to the
    /// guest's listener on `port`. `false`: the guest dials `port` and libkrun
    /// connects to the host process listening on the socket.
    pub listen: bool,
}

/// A raw disk image attached as a virtio-blk device.
#[derive(Debug, Clone)]
pub struct Disk {
    pub block_id: String,
    pub path: PathBuf,
    pub read_only: bool,
}

/// A host directory shared with the guest over virtio-fs.
#[derive(Debug, Clone)]
pub struct Share {
    /// What the guest passes to `mount -t virtiofs`.
    pub tag: String,
    pub path: PathBuf,
}

/// A virtio-net device whose backend listens on a unix datagram socket.
#[derive(Debug, Clone)]
pub struct NetDevice {
    pub socket: PathBuf,
    pub mac: [u8; 6],
    /// virtio-net feature bits.
    pub features: u32,
    /// Send the vfkit magic (gvproxy `-listen-vfkit`).
    pub vfkit: bool,
}

/// Everything libkrun needs to boot one guest.
#[derive(Debug, Clone)]
pub struct BootConfig {
    /// Host directory served as the guest's root over virtio-fs.
    pub root_dir: PathBuf,
    /// Program libkrun's init starts, relative to the guest root.
    pub exec_path: String,
    pub argv: Vec<String>,
    /// `KEY=value` entries for the guest's init.
    pub env: Vec<String>,
    /// Receives the kernel and init console output.
    pub console_log: Option<PathBuf>,
    pub vsock_ports: Vec<VsockPort>,
    /// Block devices in order: the first is `/dev/vda`.
    pub disks: Vec<Disk>,
    /// Extra virtio-fs shares (the root directory is separate).
    pub shares: Vec<Share>,
    /// virtio-net devices (`eth0`, ...); without any, libkrun uses TSI.
    pub nets: Vec<NetDevice>,
    pub cpus: u8,
    pub memory_mib: u32,
}

impl BootConfig {
    pub fn new(root_dir: impl Into<PathBuf>, exec_path: impl Into<String>) -> Self {
        BootConfig {
            root_dir: root_dir.into(),
            exec_path: exec_path.into(),
            argv: Vec::new(),
            env: Vec::new(),
            console_log: None,
            vsock_ports: Vec::new(),
            disks: Vec::new(),
            shares: Vec::new(),
            nets: Vec::new(),
            cpus: 1,
            memory_mib: 512,
        }
    }
}

/// Boots the guest and does not return on success: libkrun exits the process
/// when the guest powers off.
#[cfg(not(target_os = "macos"))]
pub fn boot(_config: &BootConfig) -> Result<std::convert::Infallible, Error> {
    Err(Error::unsupported())
}

/// Boots the guest and does not return on success: libkrun exits the process
/// when the guest powers off.
#[cfg(target_os = "macos")]
pub fn boot(config: &BootConfig) -> Result<std::convert::Infallible, Error> {
    use std::ffi::CString;

    fn cstr(s: &str, what: &str) -> Result<CString, Error> {
        CString::new(s).map_err(|_| Error::invalid_config(what))
    }
    fn path(p: &std::path::Path, what: &str) -> Result<CString, Error> {
        cstr(&p.to_string_lossy(), what)
    }
    fn check(call: &str, rc: i32) -> Result<(), Error> {
        if rc < 0 {
            Err(Error::from_krun(call, rc))
        } else {
            Ok(())
        }
    }
    fn ptrs(items: &[CString]) -> Vec<*const std::ffi::c_char> {
        items
            .iter()
            .map(|c| c.as_ptr())
            .chain(std::iter::once(std::ptr::null()))
            .collect()
    }

    hv::probe()?;

    let root = path(&config.root_dir, "root directory")?;
    let exec = cstr(&config.exec_path, "exec path")?;
    let argv: Vec<CString> = config
        .argv
        .iter()
        .map(|a| cstr(a, "argument"))
        .collect::<Result<_, _>>()?;
    let env: Vec<CString> = config
        .env
        .iter()
        .map(|a| cstr(a, "environment entry"))
        .collect::<Result<_, _>>()?;
    let argv_ptrs = ptrs(&argv);
    let env_ptrs = ptrs(&env);

    // SAFETY: every pointer passed below comes from a CString or a
    // NULL-terminated pointer array that outlives the call; ctx is the id
    // libkrun returned.
    unsafe {
        check("krun_set_log_level", ffi::krun_set_log_level(2))?;
        let ctx = ffi::krun_create_ctx();
        check("krun_create_ctx", ctx)?;
        let ctx = ctx as u32;
        check(
            "krun_set_vm_config",
            ffi::krun_set_vm_config(ctx, config.cpus, config.memory_mib),
        )?;
        check("krun_set_root", ffi::krun_set_root(ctx, root.as_ptr()))?;
        if let Some(log) = &config.console_log {
            let log = path(log, "console log path")?;
            check(
                "krun_set_console_output",
                ffi::krun_set_console_output(ctx, log.as_ptr()),
            )?;
        }
        for d in &config.disks {
            let id = cstr(&d.block_id, "block id")?;
            let disk = path(&d.path, "disk path")?;
            check(
                "krun_add_disk2",
                ffi::krun_add_disk2(ctx, id.as_ptr(), disk.as_ptr(), 0, d.read_only),
            )?;
        }
        for s in &config.shares {
            let tag = cstr(&s.tag, "share tag")?;
            let dir = path(&s.path, "share path")?;
            check(
                "krun_add_virtiofs",
                ffi::krun_add_virtiofs(ctx, tag.as_ptr(), dir.as_ptr()),
            )?;
        }
        for n in &config.nets {
            let sock = path(&n.socket, "network socket path")?;
            // NET_FLAG_VFKIT = 1 << 0.
            let flags = u32::from(n.vfkit);
            check(
                "krun_add_net_unixgram",
                ffi::krun_add_net_unixgram(
                    ctx,
                    sock.as_ptr(),
                    -1,
                    n.mac.as_ptr(),
                    n.features,
                    flags,
                ),
            )?;
        }
        for p in &config.vsock_ports {
            let sock = path(&p.host_socket, "vsock socket path")?;
            check(
                "krun_add_vsock_port2",
                ffi::krun_add_vsock_port2(ctx, p.port, sock.as_ptr(), p.listen),
            )?;
        }
        check(
            "krun_set_exec",
            ffi::krun_set_exec(ctx, exec.as_ptr(), argv_ptrs.as_ptr(), env_ptrs.as_ptr()),
        )?;
        let rc = ffi::krun_start_enter(ctx);
        // Only reached when setup failed.
        Err(Error::from_krun("krun_start_enter", rc))
    }
}
