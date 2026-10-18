//! Helpers shared by the CLI tests: the guestd stand-in, vmctl invocations
//! over a temp state directory, and polling.
#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

#[path = "../../../../tests/support/cargo.rs"]
mod cargo;
pub use cargo::build_binary as built;

pub fn guestd() -> PathBuf {
    built("clankerd-guestd")
}

/// `vmctl args...` over the state in `dir`, on the local-process VMM stand-in.
pub fn command(dir: &Path, guestd: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_vmctl"));
    c.args(args)
        .env("CLANKERD_STATE_DIR", dir.join("state"))
        .env("CLANKERD_RUNTIME_DIR", dir.join("run"))
        .env("CLANKERD_DEV_GUESTD", guestd);
    c
}

pub fn vmctl(dir: &Path, guestd: &Path, args: &[&str]) -> Output {
    command(dir, guestd, args).output().unwrap()
}

pub fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// Polls `ok` every 20 ms for up to 10 s.
pub fn wait_for(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Removes persistent machines while their test state directory still exists.
pub struct Cleanup {
    dir: PathBuf,
    guestd: PathBuf,
}

pub fn cleanup_on_drop(dir: &Path, guestd: &Path) -> Cleanup {
    Cleanup {
        dir: dir.to_owned(),
        guestd: guestd.to_owned(),
    }
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.dir.join("state/machines")) else {
            return;
        };
        for entry in entries.flatten() {
            if let Some(id) = entry.file_name().to_str() {
                let _ = vmctl(&self.dir, &self.guestd, &["rm", "-f", id]);
            }
        }
    }
}

/// A management filesystem for the local launcher; no alternate flat layout.
pub fn guest_root() -> PathBuf {
    static ROOT: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let root = tempfile::tempdir().unwrap();
        for name in ["sbin", "usr/sbin"] {
            std::fs::create_dir_all(root.path().join(name)).unwrap();
        }
        std::fs::copy(guestd(), root.path().join("usr/sbin/clankerd-guestd")).unwrap();
        for name in ["sbin/mke2fs", "sbin/dhcpcd", "usr/sbin/resize2fs"] {
            std::fs::write(root.path().join(name), "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(
                root.path().join(name),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        root
    })
    .path()
    .to_owned()
}
