//! Shared seam A harness: a real `Engine` over a temp directory whose only
//! fake is the `Vmm` (clankerd-guestd as a local process, unix sockets for vsock).
#![allow(dead_code)]

pub mod registry;
pub mod tarcompare;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use libclankerd::vmm::LocalProcessVmm;
use libclankerd::{Engine, EngineConfig, HostConfig, Machine, MachineConfig, Vmm};

/// Path of the clankerd-guestd binary, building it on first use because
/// cargo only builds a package's own binaries for its integration tests.
pub fn guestd_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
        let mut cmd = Command::new(cargo);
        cmd.args(["build", "-q", "-p", "clankerd-guestd"]);
        if profile_dir.file_name().is_some_and(|n| n == "release") {
            cmd.arg("--release");
        }
        let status = cmd.status().expect("run cargo build");
        assert!(status.success(), "building clankerd-guestd failed");
        profile_dir.join("clankerd-guestd")
    })
    .clone()
}

pub struct Env {
    pub dir: tempfile::TempDir,
}

impl Env {
    pub fn new() -> Self {
        Env {
            dir: tempfile::Builder::new().prefix("lc").tempdir().unwrap(),
        }
    }

    pub fn config(&self, vmm: Arc<dyn Vmm>) -> EngineConfig {
        let mut cfg = EngineConfig::new(self.dir.path().join("state"), self.dir.path().join("run"));
        cfg.vmm = vmm;
        cfg.start_timeout = Duration::from_secs(20);
        cfg
    }

    pub fn engine(&self) -> Engine {
        Engine::new(self.config(Arc::new(LocalProcessVmm::new(guestd_path())))).unwrap()
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }
}

pub fn sh(script: &str) -> MachineConfig {
    MachineConfig {
        image: "placeholder:latest".into(),
        entrypoint: vec!["/bin/sh".into(), "-c".into()],
        cmd: vec![script.into()],
        ..Default::default()
    }
}

pub fn create(engine: &Engine, name: &str, script: &str) -> Machine {
    engine
        .create(Some(name), sh(script), HostConfig::default())
        .unwrap()
}

/// Stands in for the population boot: stores the merged tar it is handed in the
/// "disk" file, and counts its runs.
#[derive(Default)]
pub struct TarCapture {
    pub runs: std::sync::atomic::AtomicUsize,
}

impl libclankerd::DiskPopulator for TarCapture {
    fn populate(
        &self,
        disk: &Path,
        size: u64,
        tar: &mut dyn std::io::Read,
    ) -> libclankerd::Result<()> {
        self.runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        assert!(size >= 1 << 30, "root disks leave headroom: {size}");
        let mut out = std::fs::File::create(disk)?;
        std::io::copy(tar, &mut out)?;
        Ok(())
    }
}

/// A boot directory holding the host's mke2fs (stands in for the shipped static one).
pub fn boot_dir_with_mke2fs() -> tempfile::TempDir {
    let dir = tempfile::Builder::new().prefix("boot").tempdir().unwrap();
    let mke2fs = match std::env::var_os("CLANKERD_TEST_BOOT_DIR") {
        Some(d) => PathBuf::from(d).join("mke2fs"),
        None => ["/usr/sbin/mke2fs", "/sbin/mke2fs", "/usr/bin/mke2fs"]
            .into_iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .expect("mke2fs (e2fsprogs) must be installed"),
    };
    std::os::unix::fs::symlink(mke2fs, dir.path().join("mke2fs")).unwrap();
    dir
}

pub fn is_root() -> bool {
    // SAFETY: no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Runs `e2fsck -fn` on an image and fails the test if it is not clean.
pub fn e2fsck_clean(image: &Path) {
    let out = Command::new(tool("e2fsck"))
        .arg("-fn")
        .arg(image)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "e2fsck -fn failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

pub fn debugfs(image: &Path, request: &str) -> String {
    let out = Command::new(tool("debugfs"))
        .args(["-R", request])
        .arg(image)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn tool(name: &str) -> String {
    ["/usr/sbin", "/sbin", "/usr/bin"]
        .iter()
        .map(|d| format!("{d}/{name}"))
        .find(|p| Path::new(p).exists())
        .unwrap_or_else(|| panic!("{name} (e2fsprogs) must be installed"))
}
