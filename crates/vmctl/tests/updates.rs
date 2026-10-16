//! Stopped configuration patches through the public CLI.
mod common;
use common::{guestd, text, vmctl};
use serde_json::{Value, json};

fn inspect(dir: &std::path::Path, guestd: &std::path::Path, name: &str) -> Value {
    let out = vmctl(dir, guestd, &["inspect", name]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    serde_json::from_slice::<Value>(&out.stdout).unwrap()[0].clone()
}

#[test]
fn scalar_updates_replace_only_explicitly_selected_values() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    let out = v(&[
        "create",
        "--name",
        "saved",
        "--cpus",
        "2",
        "--memory",
        "128M",
        "--restart",
        "unless-stopped",
        "--pull",
        "missing",
        "--socket",
        "host=/tmp/example.sock,guest=/run/example.sock",
        "-v",
        "data:/storage",
        "img",
        "true",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    for (flag, value, field, expected) in [
        ("--cpus", "3", "cpus", json!(3)),
        ("--memory", "256MiB", "memory", json!(268435456)),
        (
            "--restart",
            "on-failure:2",
            "restart_policy",
            json!("on-failure:2"),
        ),
        ("--restart", "no", "restart_policy", json!("no")),
        ("--pull", "never", "pull_policy", json!("never")),
        ("--pull", "always", "pull_policy", json!("always")),
    ] {
        let before = inspect(dir.path(), &guestd, "saved");
        let out = v(&["update", flag, value, "saved"]);
        assert!(out.status.success(), "{flag}: {}", text(&out.stderr));
        let after = inspect(dir.path(), &guestd, "saved");
        let mut expected_record = before;
        expected_record["host_config"][field] = expected;
        assert_eq!(after, expected_record, "{flag}");
    }
}

#[test]
fn mount_updates_remove_then_add_without_replacing_other_mounts() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&[
            "create",
            "--name",
            "saved",
            "-v",
            "/tmp:/keep:ro",
            "-v",
            "retained:/retain",
            "-v",
            "data:/old",
            "img",
            "true"
        ])
        .status
        .success()
    );
    let out = v(&[
        "update",
        "--mount-remove",
        "/old",
        "--mount-add",
        "type=volume,source=new,target=/new",
        "saved",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let info = inspect(dir.path(), &guestd, "saved");
    assert_eq!(
        info["host_config"]["mounts"],
        json!([
            {"type":"bind","source":std::fs::canonicalize("/tmp").unwrap().display().to_string(),"target":"/keep","read_only":true},
            {"type":"volume","name":"retained","target":"/retain"},
            {"type":"volume","name":"new","target":"/new"}
        ])
    );
}

#[test]
fn socket_updates_remove_then_add_without_replacing_other_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&[
            "create",
            "--name",
            "saved",
            "--socket",
            "host=/tmp/keep.sock,guest=/run/keep.sock",
            "--socket",
            "host=/tmp/old.sock,guest=/run/old.sock",
            "img",
            "true"
        ])
        .status
        .success()
    );
    let out = v(&[
        "update",
        "--socket-remove",
        "/run/old.sock",
        "--socket-add",
        "host=/tmp/new.sock,guest=/run/new.sock",
        "saved",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        inspect(dir.path(), &guestd, "saved")["host_config"]["socket_bindings"],
        json!([
            {"host_path":"/tmp/keep.sock","guest_path":"/run/keep.sock"},
            {"host_path":"/tmp/new.sock","guest_path":"/run/new.sock"}
        ])
    );
}

#[test]
fn detaching_storage_preserves_volume_data_and_host_contents() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&["volume", "create", "--size", "4M", "data"])
            .status
            .success()
    );
    let output = v(&["volume", "inspect", "data"]);
    let info: Value = serde_json::from_slice(&output.stdout).unwrap();
    let path = info[0]["path"].as_str().unwrap();
    use std::io::{Read, Write};
    std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .write_all(b"retained")
        .unwrap();
    let bind = format!("{}:/host", dir.path().display());
    std::fs::write(dir.path().join("marker"), b"retained").unwrap();
    assert!(
        v(&[
            "create",
            "--name",
            "saved",
            "-v",
            "data:/storage",
            "-v",
            &bind,
            "img",
            "true"
        ])
        .status
        .success()
    );
    assert!(
        v(&[
            "update",
            "--mount-remove",
            "/storage",
            "--mount-remove",
            "/host",
            "saved"
        ])
        .status
        .success()
    );
    assert!(v(&["rm", "saved"]).status.success());
    let mut marker = [0; 8];
    std::fs::File::open(path)
        .unwrap()
        .read_exact(&mut marker)
        .unwrap();
    assert_eq!(&marker, b"retained");
    assert_eq!(
        std::fs::read(dir.path().join("marker")).unwrap(),
        b"retained"
    );
    assert!(v(&["volume", "inspect", "data"]).status.success());
}

#[test]
fn immutable_workload_fields_are_invocation_errors_without_saved_changes() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&["create", "--name", "saved", "img", "true"])
            .status
            .success()
    );
    let before = inspect(dir.path(), &guestd, "saved");
    for flags in [
        vec!["--env", "A=value"],
        vec!["--user", "example"],
        vec!["--workdir", "/tmp"],
        vec!["--entrypoint", "sh"],
        vec!["--interactive"],
        vec!["--tty"],
        vec!["--stop-signal", "TERM"],
    ] {
        let mut args = vec!["update"];
        args.extend(flags);
        args.push("saved");
        let out = v(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
        assert_eq!(inspect(dir.path(), &guestd, "saved"), before);
    }
}

#[test]
fn collection_removal_is_idempotent_and_preserves_unrelated_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&[
            "create",
            "--name",
            "saved",
            "-v",
            "/tmp:/keep",
            "--socket",
            "host=/tmp/keep.sock,guest=/run/keep.sock",
            "img",
            "true"
        ])
        .status
        .success()
    );
    let before = inspect(dir.path(), &guestd, "saved");
    for _ in 0..2 {
        let out = v(&[
            "update",
            "--mount-remove",
            "/absent",
            "--mount-remove",
            "/absent",
            "--socket-remove",
            "/run/absent.sock",
            "saved",
        ]);
        assert!(out.status.success(), "{}", text(&out.stderr));
        assert_eq!(inspect(dir.path(), &guestd, "saved"), before);
    }
}

#[test]
fn idempotent_removal_still_requires_an_existing_stopped_machine() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert_eq!(
        v(&["update", "--mount-remove", "/absent", "missing"])
            .status
            .code(),
        Some(1)
    );
    assert!(
        v(&["run", "-d", "--name", "running", "img", "true"])
            .status
            .success()
    );
    let before = inspect(dir.path(), &guestd, "running");
    let out = v(&["update", "--socket-remove", "/run/absent.sock", "running"]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("stop it first"));
    assert_eq!(inspect(dir.path(), &guestd, "running"), before);
}

#[test]
fn duplicate_mount_targets_reject_the_patch_without_changing_saved_configuration() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&[
            "create",
            "--name",
            "saved",
            "-v",
            "/tmp:/shared",
            "img",
            "true"
        ])
        .status
        .success()
    );
    let before = inspect(dir.path(), &guestd, "saved");
    let out = v(&[
        "update",
        "--memory",
        "256M",
        "--mount-add",
        "type=bind,source=/tmp,target=/shared",
        "saved",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("duplicate mount target"));
    assert_eq!(inspect(dir.path(), &guestd, "saved"), before);
}
