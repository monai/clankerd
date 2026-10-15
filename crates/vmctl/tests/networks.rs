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
        "team",
        "--mode",
        "host-only",
        "--subnet",
        "192.168.123.0/24",
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
    assert!(v(&["network", "ls"]).status.success());
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
    let output = vmctl(dir.path(), &guestd, &["network", "ls"]);
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
