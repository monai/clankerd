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
