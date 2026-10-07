//! Helpers shared by the CLI tests: the guestd stand-in, vmctl invocations
//! over a temp state directory, and polling.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

/// Path of the clankerd-guestd binary, built on demand because cargo only
/// builds this package's binaries for these tests.
pub fn guestd() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["build", "-q", "-p", "clankerd-guestd"]);
    if profile_dir.file_name().is_some_and(|n| n == "release") {
        cmd.arg("--release");
    }
    assert!(cmd.status().unwrap().success());
    profile_dir.join("clankerd-guestd")
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
