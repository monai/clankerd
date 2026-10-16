mod common;
use common::{guestd, text, vmctl};

#[test]
fn network_cli_creates_assigns_inspects_and_removes_resources() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    let created = v(&[
        "network",
        "create",
        "--mode",
        "host-only",
        "--subnet",
        "192.168.123.0/24",
        "team",
    ]);
    assert!(created.status.success(), "{}", text(&created.stderr));
    let created = v(&[
        "create",
        "--name",
        "member",
        "--network",
        "team",
        "img",
        "true",
    ]);
    assert!(created.status.success(), "{}", text(&created.stderr));
    let output = v(&["inspect", "member"]);
    let info: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(info[0]["host_config"]["virtual_network"], "team");
    assert_eq!(info[0]["host_config"]["network_mode"], "host-only");
    assert!(!v(&["network", "rm", "team"]).status.success());
    assert!(v(&["network", "inspect", "team"]).status.success());
    assert!(v(&["network", "ls", "--output", "json"]).status.success());
    assert!(
        !v(&[
            "create",
            "--network",
            "team",
            "--isolate-peers",
            "img",
            "true"
        ])
        .status
        .success()
    );
    assert!(v(&["rm", "member"]).status.success());
    assert!(v(&["network", "rm", "team"]).status.success());
}

#[test]
fn independent_cli_processes_allocate_unique_network_subnets() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let mut children = Vec::new();
    for i in 0..8 {
        children.push(
            common::command(
                dir.path(),
                &guestd,
                &["network", "create", &format!("team-{i}")],
            )
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap(),
        );
    }
    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
    let output = vmctl(dir.path(), &guestd, &["network", "ls", "--output", "json"]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    let networks: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let addresses: std::collections::HashSet<_> = networks
        .as_array()
        .unwrap()
        .iter()
        .map(|network| network["subnet"]["address"].as_str().unwrap())
        .collect();
    assert_eq!(addresses.len(), 8);
}

fn inspect(dir: &std::path::Path, guestd: &std::path::Path, name: &str) -> serde_json::Value {
    let output = vmctl(dir, guestd, &["inspect", name]);
    assert!(output.status.success(), "{}", text(&output.stderr));
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()[0].clone()
}

#[test]
fn update_network_preserves_workload_resources_and_created_state() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    for (name, mode) in [("first", "shared"), ("second", "host-only")] {
        assert!(
            v(&["network", "create", "--mode", mode, name])
                .status
                .success()
        );
    }
    let created = v(&[
        "create",
        "--name",
        "member",
        "--network",
        "first",
        "--cpus",
        "2",
        "--memory",
        "256M",
        "-e",
        "EXAMPLE=foo",
        "-v",
        "data:/storage",
        "--restart",
        "unless-stopped",
        "img",
        "sh",
        "-c",
        "sleep 60",
    ]);
    assert!(created.status.success(), "{}", text(&created.stderr));
    let before = inspect(dir.path(), &guestd, "member");
    let updated = v(&["update", "--network", "second", "member"]);
    assert!(updated.status.success(), "{}", text(&updated.stderr));
    assert_eq!(text(&updated.stdout), "member\n");
    let mut after = inspect(dir.path(), &guestd, "member");
    assert_eq!(after["config"], before["config"]);
    assert_eq!(after["state"], before["state"]);
    assert_eq!(after["host_config"]["virtual_network"], "second");
    assert_eq!(after["host_config"]["network_mode"], "host-only");
    assert!(after["host_config"]["shared_ipv6_prefix"].is_null());
    for field in [
        "virtual_network",
        "network_mode",
        "bridge_interface",
        "shared_network",
        "shared_ipv6_prefix",
    ] {
        after["host_config"][field] = before["host_config"][field].clone();
    }
    assert_eq!(after["host_config"], before["host_config"]);
    assert!(v(&["network", "rm", "first"]).status.success());
    assert!(!v(&["network", "rm", "second"]).status.success());
}

#[test]
fn update_network_requires_stopped_membership_and_persists_across_starts() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    for (name, mode) in [("first", "shared"), ("second", "host-only")] {
        assert!(
            v(&["network", "create", "--mode", mode, name])
                .status
                .success()
        );
    }
    assert!(
        v(&[
            "run",
            "-d",
            "--name",
            "member",
            "--network",
            "first",
            "img",
            "sleep",
            "60"
        ])
        .status
        .success()
    );
    let before = inspect(dir.path(), &guestd, "member");
    let rejected = v(&["update", "--network", "second", "member"]);
    assert!(!rejected.status.success());
    assert!(text(&rejected.stderr).contains("stop it first"));
    assert_eq!(
        inspect(dir.path(), &guestd, "member")["host_config"],
        before["host_config"]
    );
    for (name, mode) in [("second", "host-only"), ("first", "shared")] {
        assert!(v(&["stop", "member"]).status.success());
        let changed = v(&["update", "--network", name, "member"]);
        assert!(changed.status.success(), "{}", text(&changed.stderr));
        let assigned = inspect(dir.path(), &guestd, "member")["host_config"].clone();
        assert_eq!(assigned["virtual_network"], name);
        assert_eq!(assigned["network_mode"], mode);
        for _ in 0..2 {
            let started = v(&["start", "member"]);
            assert!(started.status.success(), "{}", text(&started.stderr));
            assert_eq!(
                inspect(dir.path(), &guestd, "member")["host_config"],
                assigned
            );
            assert!(v(&["stop", "member"]).status.success());
        }
        assert!(v(&["start", "member"]).status.success());
    }
    assert!(v(&["rm", "-f", "member"]).status.success());
}

#[test]
fn update_network_rejects_invalid_requests_without_changing_saved_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&["create", "--name", "member", "img", "true"])
            .status
            .success()
    );
    let before = inspect(dir.path(), &guestd, "member");
    for args in [
        vec!["update", "--network", "missing", "member"],
        vec!["update", "member"],
        vec!["update", "--network", "missing"],
        vec![
            "update",
            "--network",
            "missing",
            "--network-mode",
            "bridged",
            "member",
        ],
    ] {
        let rejected = v(&args);
        let expected = if args == ["update", "--network", "missing", "member"] {
            1
        } else {
            2
        };
        assert_eq!(
            rejected.status.code(),
            Some(expected),
            "{args:?}: {}",
            text(&rejected.stderr)
        );
        assert_eq!(
            inspect(dir.path(), &guestd, "member")["host_config"],
            before["host_config"]
        );
    }
    assert!(v(&["network", "create", "target"]).status.success());
    let missing = v(&["update", "--network", "target", "missing"]);
    assert!(!missing.status.success());
    assert!(text(&missing.stderr).contains("no such machine"));
}

#[test]
fn update_network_clears_the_previous_physical_attachment() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&["network", "create", "--mode", "host-only", "target"])
            .status
            .success()
    );
    assert!(
        v(&[
            "create",
            "--name",
            "member",
            "--bridge-interface",
            "en0",
            "img",
            "true"
        ])
        .status
        .success()
    );
    let updated = v(&["update", "--network", "target", "member"]);
    assert!(updated.status.success(), "{}", text(&updated.stderr));
    let after = inspect(dir.path(), &guestd, "member");
    assert_eq!(after["host_config"]["virtual_network"], "target");
    assert_eq!(after["host_config"]["network_mode"], "host-only");
    assert!(after["host_config"]["bridge_interface"].is_null());
}

#[test]
fn update_network_reports_each_machine_and_continues_after_a_missing_member() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(v(&["network", "create", "target"]).status.success());
    for name in ["first", "second"] {
        assert!(
            v(&["create", "--name", name, "img", "true"])
                .status
                .success()
        );
    }
    let result = v(&[
        "update",
        "--network",
        "target",
        "first",
        "missing",
        "second",
    ]);
    assert_eq!(result.status.code(), Some(1));
    assert_eq!(text(&result.stdout), "first\nsecond\n");
    assert!(text(&result.stderr).contains("no such machine"));
    for name in ["first", "second"] {
        assert_eq!(
            inspect(dir.path(), &guestd, name)["host_config"]["virtual_network"],
            "target"
        );
    }
}
