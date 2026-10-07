//! CLI: `--host-gateway-port` makes a host loopback port reachable from the
//! guest's loopback (ticket 10 smoke test needs it). The VMM is the
//! local-process stand-in, so this checks what the CLI hands the library.

use std::path::PathBuf;
use std::process::{Command, Output};

fn guestd() -> PathBuf {
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
fn host_gateway_ports_are_recorded_in_the_machine_config() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);

    let created = v(&[
        "create",
        "--name",
        "gw",
        "--host-gateway-port",
        "9000",
        "--host-gateway-port",
        "9001",
        "img",
        "true",
    ]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let inspect: serde_json::Value =
        serde_json::from_slice(&v(&["inspect", "gw"]).stdout).expect("inspect prints JSON");
    let text = inspect.to_string();
    assert!(
        text.contains("\"host_gateway_ports\":[9000,9001]"),
        "{text}"
    );
}

#[test]
fn host_gateway_port_zero_is_rejected() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let out = vmctl(
        dir.path(),
        &guestd,
        &["create", "--host-gateway-port", "0", "img", "true"],
    );
    assert!(!out.status.success());
}
