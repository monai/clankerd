use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use clankerd_proto::spawn::{BOOT_WORKLOAD, NetDevice, SpawnSpec};

use super::{BootHandle, BootSpec, Vmm};
use crate::error::{Error, Result};

const SPEC_FILE: &str = "vmspawn.json";
const LOG_FILE: &str = "vmspawn.log";
const STATUS_FILE: &str = "vmspawn-exit";
const DEFAULT_CPUS: u32 = 2;
const DEFAULT_MEMORY: u64 = 1024 * 1024 * 1024;

/// Boots a machine through one `clankerd-vmspawn` helper process.
///
/// The machine directory gets a `boot/` directory (guestd plus its workload),
/// which the helper serves as the guest's root over virtio-fs. The guest
/// socket is created here and handed to the helper with `LISTEN_FDS`; the
/// helper runs in its own process group so it outlives this process.
#[derive(Debug, Clone)]
pub struct VmspawnVmm {
    vmspawn: PathBuf,
    guest_root: PathBuf,
    dev_local: bool,
}

impl VmspawnVmm {
    /// `vmspawn` is the host launcher; `guest_root` is the complete management filesystem.
    pub fn new(vmspawn: impl Into<PathBuf>, guest_root: impl Into<PathBuf>) -> Self {
        VmspawnVmm {
            vmspawn: vmspawn.into(),
            guest_root: guest_root.into(),
            dev_local: false,
        }
    }

    /// Development: the helper runs guestd as a local process instead of
    /// booting a VM, so the full path works on Linux.
    pub fn dev_local(mut self) -> Self {
        self.dev_local = true;
        self
    }

    fn prepare_boot_dir(&self, boot: &Path, spec: &BootSpec) -> Result<()> {
        fs::create_dir_all(boot)?;
        crate::guest_environment::place(&self.guest_root, boot)?;
        fs::create_dir_all(boot.join("etc/clankerd"))?;
        fs::write(
            boot.join(BOOT_WORKLOAD),
            serde_json::to_vec(&spec.workload)?,
        )?;
        Ok(())
    }
}

impl Vmm for VmspawnVmm {
    fn owns_forwarding(&self) -> bool {
        true
    }
    fn boot(&self, spec: &BootSpec) -> Result<BootHandle> {
        if let Some(disk) = &spec.root_disk {
            crate::rootdisk::restore_root_capacity(disk, &spec.dir)?;
        }
        let boot = spec.dir.join("boot");
        self.prepare_boot_dir(&boot, spec)?;

        let spawn_spec = SpawnSpec {
            forwarding: Some(spec.forwarding.clone()),
            boot_dir: boot,
            console_log: spec.dir.join(crate::machine::CONSOLE_LOG),
            exit_file: spec.exit_file.clone(),
            vsock_socket: spec.guest_socket.with_extension("vsock"),
            host_socket: spec.host_socket.clone(),
            cpus: spec.cpus.unwrap_or(DEFAULT_CPUS).min(255) as u8,
            memory_mib: (spec.memory.unwrap_or(DEFAULT_MEMORY) / (1024 * 1024)).max(128) as u32,
            root_disk: spec.root_disk.clone(),
            populate: false,
            volume_disks: spec.volume_disks.clone(),
            shares: spec.shares.clone(),
            net: spec.net.as_ref().map(|n| NetDevice {
                socket: n.socket.clone(),
                mac: n.mac,
                features: n.features,
                vfkit: n.vfkit,
            }),
        };
        let spec_file = spec.dir.join(SPEC_FILE);
        fs::write(&spec_file, serde_json::to_vec_pretty(&spawn_spec)?)?;

        match fs::remove_file(&spawn_spec.vsock_socket) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(Error::system(format!(
                    "removing stale libkrun listener {}: {e}",
                    spawn_spec.vsock_socket.display()
                )));
            }
        }
        let _ = fs::remove_file(&spec.guest_socket);
        let _ = fs::remove_file(spec.dir.join(STATUS_FILE));
        let listener = UnixListener::bind(&spec.guest_socket)
            .map_err(|e| Error::system(format!("binding {}: {e}", spec.guest_socket.display())))?;
        // Keep the diagnostic file open: a late reaper write must not create a
        // directory entry after removal, or overwrite a later run's status file.
        let mut status_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(spec.dir.join(STATUS_FILE))?;
        let mut child = spawn_helper(
            &self.vmspawn,
            &spec_file,
            &spec.dir.join(LOG_FILE),
            &listener,
            self.dev_local,
        )?;
        let pid = child.id();
        // Reap it if it ends while we are still alive.
        std::thread::spawn(move || {
            if let Ok(status) = child.wait() {
                let _ = status_file.write_all(status.to_string().as_bytes());
            }
        });
        Ok(BootHandle { pid })
    }

    fn check_alive(&self, spec: &BootSpec, handle: &BootHandle) -> Result<()> {
        if self.is_alive(handle) {
            return Ok(());
        }
        let log = fs::read_to_string(spec.dir.join(LOG_FILE)).unwrap_or_default();
        let console =
            fs::read_to_string(spec.dir.join(crate::machine::CONSOLE_LOG)).unwrap_or_default();
        let error = if log.trim().is_empty() && !console.trim().is_empty() {
            let lines: Vec<_> = console.lines().collect();
            Error::unavailable(format!(
                "guest exited before becoming ready (guest console: {})",
                lines[lines.len().saturating_sub(20)..].join(" | ")
            ))
        } else {
            explain_exit(&log)
        };
        let status = fs::read_to_string(spec.dir.join(STATUS_FILE)).unwrap_or_default();
        Err(Error::new(
            error.kind(),
            format!(
                "workload boot: {}{}",
                error.message(),
                if status.is_empty() {
                    String::new()
                } else {
                    format!(" (helper {status})")
                }
            ),
        ))
    }
}

/// Starts `clankerd-vmspawn --spec spec_file` in its own process group, with
/// `listener` as its guest endpoint (`LISTEN_FDS`) and stdout/stderr going to
/// `log_path`.
pub(crate) fn spawn_helper(
    vmspawn: &Path,
    spec_file: &Path,
    log_path: &Path,
    listener: &UnixListener,
    dev_local: bool,
) -> Result<Child> {
    let fd = listener.as_raw_fd();
    let log = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(log_path)?;
    let mut cmd = Command::new(vmspawn);
    cmd.arg("--spec").arg(spec_file);
    if dev_local {
        cmd.arg("--dev-local");
    }
    cmd.env("LISTEN_FDS", "1")
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log)
        .process_group(0);
    // SAFETY: only async-signal-safe calls (dup2, fcntl) between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if fd == 3 {
                let flags = libc::fcntl(3, libc::F_GETFD);
                libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            } else if libc::dup2(fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| {
        Error::unavailable(format!(
            "cannot start clankerd-vmspawn ({}): {e}",
            vmspawn.display()
        ))
    })
}

/// Turns the helper's captured stderr into what the developer should read.
pub(crate) fn explain_exit(log: &str) -> Error {
    if log.contains("Library not loaded") {
        return Error::unavailable(
            "bundled host library could not be loaded; rebuild the complete artifact (see vmspawn.log)",
        );
    }
    let tail: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &tail[tail.len().saturating_sub(5)..];
    if tail.is_empty() {
        Error::unavailable(
            "clankerd-vmspawn exited before the guest became ready and printed nothing \
             (it may have crashed; see vmspawn.log in the machine directory)",
        )
    } else {
        Error::unavailable(format!(
            "clankerd-vmspawn exited before the guest became ready: {}",
            tail.join(" | ")
        ))
    }
}
