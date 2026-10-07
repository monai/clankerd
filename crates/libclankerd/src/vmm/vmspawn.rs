use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use clankerd_proto::spawn::{BOOT_GUESTD, BOOT_WORKLOAD, SpawnSpec};

use super::{BootHandle, BootSpec, Vmm};
use crate::error::{Error, Result};

const SPEC_FILE: &str = "vmspawn.json";
const LOG_FILE: &str = "vmspawn.log";
/// Static binaries guestd needs for a volume, next to guestd.
const VOLUME_TOOLS: [&str; 2] = ["mke2fs", "resize2fs"];
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
    guestd: PathBuf,
    dev_local: bool,
}

impl VmspawnVmm {
    /// `vmspawn` is the helper binary; `guestd` the static aarch64 Linux
    /// clankerd-guestd that becomes PID 1 in the guest.
    pub fn new(vmspawn: impl Into<PathBuf>, guestd: impl Into<PathBuf>) -> Self {
        VmspawnVmm {
            vmspawn: vmspawn.into(),
            guestd: guestd.into(),
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
        let target = boot.join(BOOT_GUESTD);
        let _ = fs::remove_file(&target);
        // virtio-fs cannot follow links out of the shared directory, so the
        // binary has to be a real file here.
        if fs::hard_link(&self.guestd, &target).is_err() {
            fs::copy(&self.guestd, &target).map_err(|e| {
                Error::unavailable(format!(
                    "cannot place clankerd-guestd ({}) in the boot directory: {e}",
                    self.guestd.display()
                ))
            })?;
        }
        if spec.volume_disk.is_some() {
            self.place_volume_tools(boot)?;
        }
        fs::write(
            boot.join(BOOT_WORKLOAD),
            serde_json::to_vec(&spec.workload)?,
        )?;
        Ok(())
    }

    /// guestd formats and grows the volume with the static e2fsprogs that ship
    /// next to it; they must be in the boot directory it keeps at
    /// `/run/clankerd/boot`.
    fn place_volume_tools(&self, boot: &Path) -> Result<()> {
        let dir = self.guestd.parent().unwrap_or(Path::new("."));
        for name in VOLUME_TOOLS {
            let src = dir.join(name);
            if !src.exists() {
                return Err(Error::unavailable(format!(
                    "{name} not found in {} (volumes need it; run `make rust`, it builds e2fsprogs)",
                    dir.display()
                )));
            }
            let dst = boot.join(name);
            let _ = fs::remove_file(&dst);
            if fs::hard_link(&src, &dst).is_err() {
                fs::copy(&src, &dst).map_err(|e| {
                    Error::unavailable(format!("cannot place {name} in the boot directory: {e}"))
                })?;
            }
        }
        Ok(())
    }
}

impl Vmm for VmspawnVmm {
    fn boot(&self, spec: &BootSpec) -> Result<BootHandle> {
        let boot = spec.dir.join("boot");
        self.prepare_boot_dir(&boot, spec)?;

        let spawn_spec = SpawnSpec {
            boot_dir: boot,
            console_log: spec.dir.join(crate::machine::CONSOLE_LOG),
            exit_file: spec.exit_file.clone(),
            vsock_socket: spec.guest_socket.with_extension("vsock"),
            host_socket: spec.host_socket.clone(),
            cpus: spec.cpus.unwrap_or(DEFAULT_CPUS).min(255) as u8,
            memory_mib: (spec.memory.unwrap_or(DEFAULT_MEMORY) / (1024 * 1024)).max(128) as u32,
            root_disk: spec.root_disk.clone(),
            populate: false,
            volume_disk: spec.volume_disk.clone(),
            shares: spec.shares.clone(),
        };
        let spec_file = spec.dir.join(SPEC_FILE);
        fs::write(&spec_file, serde_json::to_vec_pretty(&spawn_spec)?)?;

        let _ = fs::remove_file(&spec.guest_socket);
        let listener = UnixListener::bind(&spec.guest_socket)
            .map_err(|e| Error::system(format!("binding {}: {e}", spec.guest_socket.display())))?;
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
            let _ = child.wait();
        });
        Ok(BootHandle { pid })
    }

    fn check_alive(&self, spec: &BootSpec, handle: &BootHandle) -> Result<()> {
        // SAFETY: signal 0 only checks that the process exists.
        let alive = unsafe { libc::kill(handle.pid as i32, 0) } == 0;
        if alive {
            return Ok(());
        }
        let log = fs::read_to_string(spec.dir.join(LOG_FILE)).unwrap_or_default();
        Err(explain_exit(&log))
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
        if log.contains("libkrunfw") {
            return Error::unavailable(
                "libkrunfw (the guest kernel library) is not installed; \
                 run `brew install libkrunfw`",
            );
        }
        if log.contains("libkrun") {
            return Error::unavailable(
                "libkrun is not installed; run `brew tap slp/krun && brew install libkrun` \
                 (libkrun 1.19.x, expected at /opt/homebrew/lib/libkrun.1.dylib)",
            );
        }
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
