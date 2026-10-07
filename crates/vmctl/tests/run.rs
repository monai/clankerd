//! CLI smoke: `vmctl run` returns the command's exit code; ps/inspect/rm work.
//! The VMM is the local-process stand-in (see libclankerd::vmm).

mod common;

use std::path::PathBuf;
use std::process::Command;

use common::{guestd, vmctl};

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
        .env("CLANKERD_BOOT_DIR_ROOT", "true")
        .env("CLANKERD_VMSPAWN", "/no/such/clankerd-vmspawn")
        .env("CLANKERD_GUESTD", guestd())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(125));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("/no/such/clankerd-vmspawn"), "{err}");
}

#[test]
fn run_dash_p_publishes_a_guest_port_on_host_loopback() {
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};
    use std::time::{Duration, Instant};

    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();

    // The stand-in guest's loopback is 127.0.0.2.
    let service = TcpListener::bind("127.0.0.2:0").unwrap();
    let guest_port = service.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in service.incoming().flatten() {
            let mut seen = String::new();
            c.read_to_string(&mut seen).unwrap();
            write!(c, "guest saw {seen}").unwrap();
        }
    });
    let host_port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();

    let mut run = Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .args(["run", "-p", &format!("{host_port}:{guest_port}")])
        .args(["img", "sleep", "3"])
        .env("CLANKERD_STATE_DIR", dir.path().join("state"))
        .env("CLANKERD_RUNTIME_DIR", dir.path().join("run"))
        .env("CLANKERD_DEV_GUESTD", &guestd)
        .spawn()
        .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut conn = loop {
        match TcpStream::connect(("127.0.0.1", host_port)) {
            Ok(c) => break c,
            Err(e) => {
                assert!(Instant::now() < deadline, "never published: {e}");
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    conn.write_all(b"ping").unwrap();
    conn.shutdown(Shutdown::Write).unwrap();
    let mut out = String::new();
    conn.read_to_string(&mut out).unwrap();
    assert_eq!(out, "guest saw ping");
    run.wait().unwrap();
}

#[test]
fn run_dash_p_refuses_non_loopback_addresses() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let out = vmctl(
        dir.path(),
        &guestd(),
        &["run", "-p", "0.0.0.0:8080:80", "img", "true"],
    );
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("loopback"));
}

#[test]
fn cpus_and_memory_are_recorded_in_the_machine_config() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    let created = v(&[
        "create", "--name", "sized", "--cpus", "2", "--memory", "512M", "img", "true",
    ]);
    assert!(created.status.success(), "{created:?}");
    let inspect = String::from_utf8(v(&["inspect", "sized"]).stdout).unwrap();
    let info: serde_json::Value = serde_json::from_str(&inspect).unwrap();
    assert_eq!(info[0]["host_config"]["cpus"], 2);
    assert_eq!(info[0]["host_config"]["memory"], 512 * 1024 * 1024);

    for bad in [["--cpus", "0"], ["--memory", "lots"]] {
        let out = v(&["create", bad[0], bad[1], "img", "true"]);
        assert!(!out.status.success(), "{bad:?}");
    }
}
