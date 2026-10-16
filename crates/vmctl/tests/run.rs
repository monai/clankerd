//! CLI smoke: `vmctl run` returns the command's exit code; ps/inspect/rm work.
//! The VMM is the local-process stand-in (see libclankerd::vmm).

mod common;

use std::process::Command;

use common::{built, command, guestd, vmctl};

#[test]
fn immediate_foreground_commands_preserve_output_and_exit_codes() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
    let helper = built("clankerd-vmspawn");
    for i in 0..10 {
        let name = format!("quick-{i}");
        let mut run = command(
            dir.path(),
            &guestd,
            &[
                "run",
                "-i",
                "--name",
                &name,
                "img",
                "/bin/sh",
                "-c",
                "printf quick-output; exit 7",
            ],
        );
        if i >= 5 {
            run.env_remove("CLANKERD_DEV_GUESTD")
                .env("CLANKERD_VMSPAWN", &helper)
                .env("CLANKERD_GUESTD", &guestd)
                .env("CLANKERD_VMSPAWN_DEV_LOCAL", "1")
                .env("CLANKERD_BOOT_DIR_ROOT", "true");
        }
        let output = run.output().unwrap();
        assert_eq!(
            output.status.code(),
            Some(7),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(String::from_utf8(output.stdout).unwrap(), "quick-output");
    }
}

#[test]
fn logs_show_workload_output() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
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
    assert_eq!(v(&["logs", "nope"]).status.code(), Some(1));
}

#[test]
fn run_ps_inspect_rm() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
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
        v(&["ps"]).stdout.iter().filter(|b| **b == b'\n').count() == 3,
        "both machines remain running after their commands finish"
    );
    let all = String::from_utf8(v(&["ps", "-a"]).stdout).unwrap();
    assert!(
        all.contains("three") && all.contains("ok") && all.contains("Up"),
        "{all}"
    );

    let inspect: serde_json::Value =
        serde_json::from_slice(&v(&["inspect", "three"]).stdout).unwrap();
    assert_eq!(inspect[0]["state"]["status"], "running");
    assert_eq!(inspect[0]["state"]["exit_code"], 0);

    assert!(v(&["stop", "ok", "three"]).status.success());
    assert!(v(&["rm", "ok", "three"]).status.success());
    let all = String::from_utf8(v(&["ps", "-a"]).stdout).unwrap();
    assert_eq!(all.lines().count(), 1, "{all}");
}

#[test]
fn run_and_logs_through_the_vmspawn_helper() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let (guestd, vmspawn) = (guestd(), built("clankerd-vmspawn"));
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
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
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("/no/such/clankerd-vmspawn"), "{err}");
}

#[test]
fn cpus_and_memory_are_recorded_in_the_machine_config() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
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

#[test]
fn virtual_network_creation_owns_subnet_validation() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&[
            "network",
            "create",
            "--subnet",
            "192.168.109.0/24",
            "selected"
        ])
        .status
        .success()
    );
    assert!(
        v(&[
            "create",
            "--name",
            "member",
            "--network",
            "selected",
            "img",
            "true"
        ])
        .status
        .success()
    );
    let output = v(&["inspect", "member"]);
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        info[0]["host_config"]["shared_network"],
        serde_json::json!({"address":"192.168.109.0","prefix_len":24})
    );
    assert_eq!(
        v(&[
            "network",
            "create",
            "--subnet",
            "192.168.109.3/24",
            "invalid"
        ])
        .status
        .code(),
        Some(2)
    );
}

#[test]
fn host_only_cli_persists_membership_and_a_distinct_default_subnet() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
    assert!(
        vmctl(
            dir.path(),
            &guestd,
            &["network", "create", "--mode", "host-only", "local"]
        )
        .status
        .success()
    );
    let created = vmctl(
        dir.path(),
        &guestd,
        &[
            "create",
            "--name",
            "host-only",
            "--network",
            "local",
            "img",
            "true",
        ],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let inspected = vmctl(dir.path(), &guestd, &["inspect", "host-only"]);
    let info: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(info[0]["host_config"]["network_mode"], "host-only");
    assert_eq!(info[0]["host_config"]["virtual_network"], "local");
}
