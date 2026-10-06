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
