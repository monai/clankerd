//! The VM-backed [`DiskPopulator`]: a *population boot* through the
//! `clankerd-vmspawn` helper.
//!
//! The helper boots a VM whose root is a throwaway boot directory holding
//! clankerd-guestd and the static e2fsprogs, with the blank disk attached as
//! the first block device. guestd runs with `--populate`, the library streams
//! it the merged tar over the usual guest socket (`PopulateDisk`), guestd
//! formats the disk, unpacks the tar on it and powers the VM off. Nothing
//! filesystem-level runs on the host.
//!
//! On Linux the same path runs with the helper's `--dev-local`: guestd is a
//! local process and a directory stands in for the block device.

use std::fs::{self, File};
use std::io::Read;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant};

use clankerd_proto::spawn::{BOOT_GUESTD, ROOT_DEVICE, SpawnSpec};

use crate::error::{Error, Result};
use crate::guest::{EventStream, Next};
use crate::rootdisk::{DiskPopulator, populate_over};
use crate::vmm::explain_exit;

const SPEC_FILE: &str = "vmspawn.json";
const LOG_FILE: &str = "vmspawn.log";
const CONSOLE_FILE: &str = "console.log";
/// Static binaries of the guest boot directory the population boot needs.
const REQUIRED: [&str; 2] = [BOOT_GUESTD, "mke2fs"];
/// Everything else in the shipped directory that guestd may use later.
const OPTIONAL: [&str; 3] = ["e2fsck", "resize2fs", "debugfs"];
const BOOT_TIMEOUT: Duration = Duration::from_secs(120);
const POWER_OFF_TIMEOUT: Duration = Duration::from_secs(30);

/// Builds base root disks with a VM booted by clankerd-vmspawn.
#[derive(Debug, Clone)]
pub struct VmspawnPopulator {
    vmspawn: PathBuf,
    guest_dir: PathBuf,
    dev_local: bool,
}

impl VmspawnPopulator {
    /// `vmspawn` is the helper binary; `guest_dir` the directory shipping the
    /// static aarch64 `clankerd-guestd` and e2fsprogs (`mke2fs`, ...), e.g.
    /// `build/rust/linux-arm64`.
    pub fn new(vmspawn: impl Into<PathBuf>, guest_dir: impl Into<PathBuf>) -> Self {
        VmspawnPopulator {
            vmspawn: vmspawn.into(),
            guest_dir: guest_dir.into(),
            dev_local: false,
        }
    }

    /// Development: the helper runs guestd as a local process instead of
    /// booting a VM, and `disk` may be a directory.
    pub fn dev_local(mut self) -> Self {
        self.dev_local = true;
        self
    }

    fn prepare_boot_dir(&self, boot: &Path) -> Result<()> {
        fs::create_dir_all(boot)?;
        for name in REQUIRED {
            if !self.guest_dir.join(name).exists() {
                return Err(Error::unavailable(format!(
                    "{name} not found in {} (run `make rust`, it builds the guest boot directory)",
                    self.guest_dir.display()
                )));
            }
        }
        for name in REQUIRED.into_iter().chain(OPTIONAL) {
            let src = self.guest_dir.join(name);
            if !src.exists() {
                continue;
            }
            let dst = boot.join(name);
            // virtio-fs cannot follow links out of the shared directory.
            if fs::hard_link(&src, &dst).is_err() {
                fs::copy(&src, &dst).map_err(|e| {
                    Error::unavailable(format!("cannot place {} in the boot directory: {e}", name))
                })?;
            }
        }
        Ok(())
    }
}

impl DiskPopulator for VmspawnPopulator {
    fn populate(&self, disk: &Path, size: u64, tar: &mut dyn Read) -> Result<()> {
        // Short names: unix socket paths are limited to ~100 bytes.
        let work = tempfile::Builder::new().prefix("clankerd-pop").tempdir()?;
        let boot = work.path().join("boot");
        self.prepare_boot_dir(&boot)?;

        // A real block device has the disk file's size.
        if fs::metadata(disk)?.is_file() {
            let len = fs::metadata(disk)?.len();
            File::options()
                .write(true)
                .open(disk)?
                .set_len(size.max(len))?;
        }

        let socket = work.path().join("g.sock");
        let spec = SpawnSpec {
            boot_dir: boot,
            console_log: work.path().join(CONSOLE_FILE),
            exit_file: work.path().join("exit"),
            vsock_socket: work.path().join("g.vsock"),
            host_socket: work.path().join("h.sock"),
            cpus: 2,
            memory_mib: 1024,
            root_disk: Some(disk.to_path_buf()),
            populate: true,
            volume_disk: None,
            shares: Vec::new(),
            net: None,
        };
        let spec_file = work.path().join(SPEC_FILE);
        fs::write(&spec_file, serde_json::to_vec_pretty(&spec)?)?;

        let listener = UnixListener::bind(&socket)
            .map_err(|e| Error::system(format!("binding {}: {e}", socket.display())))?;
        let log = work.path().join(LOG_FILE);
        let mut child =
            crate::vmm::spawn_helper(&self.vmspawn, &spec_file, &log, &listener, self.dev_local)?;
        // The helper holds the only descriptor now; if it dies, connecting fails fast.
        drop(listener);

        let result = (|| {
            wait_ready(&socket, &mut child, &log)?;
            // In a VM the disk is the guest's first block device; in the
            // stand-in the guest sees the host path.
            let device = if self.dev_local {
                disk.to_path_buf()
            } else {
                PathBuf::from(ROOT_DEVICE)
            };
            populate_over(&socket, &device, size, tar)?;
            wait_for_power_off(&mut child)
        })();
        if let Err(e) = &result {
            kill(&mut child);
            // Whatever the guest printed helps most when the unpack failed.
            let console = fs::read_to_string(work.path().join(CONSOLE_FILE)).unwrap_or_default();
            let tail: Vec<&str> = console.lines().rev().take(5).collect();
            if !tail.is_empty() && !e.message().contains("clankerd-vmspawn exited") {
                let tail: Vec<&str> = tail.into_iter().rev().collect();
                return Err(Error::new(
                    e.kind(),
                    format!("{} (guest console: {})", e.message(), tail.join(" | ")),
                ));
            }
        }
        result
    }
}

/// Waits until guestd answers on the guest socket, failing fast with the
/// helper's own explanation if it dies first.
fn wait_ready(socket: &Path, child: &mut Child, log: &Path) -> Result<()> {
    let deadline = Instant::now() + BOOT_TIMEOUT;
    loop {
        if let Ok(mut stream) = EventStream::connect(socket)
            && matches!(
                // A listener nobody serves (the helper died) accepts and stays silent.
                stream.next(Some(
                    deadline.min(Instant::now() + Duration::from_millis(500))
                )),
                Next::Event(clankerd_proto::guest::Event::Ready)
            )
        {
            return Ok(());
        }
        if let Ok(Some(_)) = child.try_wait() {
            return Err(explain_exit(&fs::read_to_string(log).unwrap_or_default()));
        }
        if Instant::now() >= deadline {
            return Err(Error::unavailable(format!(
                "timed out after {BOOT_TIMEOUT:?} waiting for the population boot"
            )));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// After the reply the guest powers off and the helper exits with it.
fn wait_for_power_off(child: &mut Child) -> Result<()> {
    let deadline = Instant::now() + POWER_OFF_TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::unavailable(
                "the population boot did not power off after unpacking",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn kill(child: &mut Child) {
    // SAFETY: plain signal delivery to the helper's process group.
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
    let _ = child.kill();
    let _ = child.wait();
}
