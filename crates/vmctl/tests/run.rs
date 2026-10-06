//! CLI smoke: `vmctl run` returns the command's exit code; ps/inspect/rm work.
//! The VMM is the local-process stand-in (see libclankerd::vmm).

use std::path::PathBuf;
use std::process::{Command, Output};

fn guestd() -> PathBuf {
    // Cargo builds only this package's binaries for these tests.
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

fn vmctl(dir: &std::path::Path, guestd: &std::path::Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .args(args)
        .env("CLANKERD_STATE_DIR", dir.join("state"))
        .env("CLANKERD_RUNTIME_DIR", dir.join("run"))
        .env("CLANKERD_DEV_GUESTD", guestd)
        .output()
        .unwrap()
}

#[test]
fn logs_show_workload_output() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    let run = [
        "run",
        "--name",
        "talker",
        "img",
        "sh",
        "-c",
        "echo from-guest",
    ];
    assert!(v(&run).status.success());
    let out = v(&["logs", "talker"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("from-guest"));
    assert_eq!(v(&["logs", "nope"]).status.code(), Some(125));
}

#[test]
fn run_ps_inspect_rm() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);

    assert_eq!(
        v(&["run", "--name", "ok", "img", "sh", "-c", "exit 0"])
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        v(&["run", "--name", "three", "img", "sh", "-c", "exit 3"])
            .status
            .code(),
        Some(3)
    );

    assert!(
        v(&["ps"]).stdout.iter().filter(|b| **b == b'\n').count() == 1,
        "ps shows only the header"
    );
    let all = String::from_utf8(v(&["ps", "-a"]).stdout).unwrap();
    assert!(
        all.contains("Exited (3)") && all.contains("Exited (0)"),
        "{all}"
    );

    let inspect: serde_json::Value =
        serde_json::from_slice(&v(&["inspect", "three"]).stdout).unwrap();
    assert_eq!(inspect[0]["state"]["status"], "exited");
    assert_eq!(inspect[0]["state"]["exit_code"], 3);

    assert!(v(&["rm", "ok", "three"]).status.success());
    let all = String::from_utf8(v(&["ps", "-a"]).stdout).unwrap();
    assert_eq!(all.lines().count(), 1, "{all}");
}

fn built(package: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["build", "-q", "-p", package]);
    if profile_dir.file_name().is_some_and(|n| n == "release") {
        cmd.arg("--release");
    }
    assert!(cmd.status().unwrap().success());
    profile_dir.join(package)
}

#[test]
fn run_and_logs_through_the_vmspawn_helper() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let (guestd, vmspawn) = (guestd(), built("clankerd-vmspawn"));
    let v = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_vmctl"))
            .args(args)
            .env("CLANKERD_STATE_DIR", dir.path().join("state"))
            .env("CLANKERD_RUNTIME_DIR", dir.path().join("run"))
            .env("CLANKERD_VMSPAWN", &vmspawn)
            .env("CLANKERD_GUESTD", &guestd)
            .env("CLANKERD_VMSPAWN_DEV_LOCAL", "1")
            .output()
            .unwrap()
    };
    let run = [
        "run",
        "--name",
        "m",
        "img",
        "sh",
        "-c",
        "echo hi-vm; exit 4",
    ];
    assert_eq!(v(&run).status.code(), Some(4));
    let logs = v(&["logs", "m"]);
    assert!(String::from_utf8_lossy(&logs.stdout).contains("hi-vm"));
}

#[test]
fn a_missing_helper_binary_is_reported_by_path() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .args(["run", "img", "true"])
        .env("CLANKERD_STATE_DIR", dir.path().join("state"))
        .env("CLANKERD_RUNTIME_DIR", dir.path().join("run"))
        .env("CLANKERD_VMSPAWN", "/no/such/clankerd-vmspawn")
        .env("CLANKERD_GUESTD", guestd())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(125));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("/no/such/clankerd-vmspawn"), "{err}");
}
