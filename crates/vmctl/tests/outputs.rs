//! Result records and batch failure contracts at the executable boundary.
mod common;
use common::{guestd, text, vmctl};
use serde_json::{Value, json};

#[test]
fn batch_mutations_associate_each_outcome_with_its_input_and_diagnostic() {
    for resource in ["machine", "network", "volume"] {
        let dir = tempfile::tempdir().unwrap();
        let guestd = guestd();
        let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
        for name in ["first", "last"] {
            let mut args = vec![resource, "create"];
            if resource == "machine" {
                args.extend(["--name", name, "img", "true"]);
            } else {
                args.push(name);
            }
            let out = v(&args);
            assert!(out.status.success(), "{}", text(&out.stderr));
        }
        let out = v(&[
            resource, "rm", "--output", "json", "first", "missing", "last",
        ]);
        assert_eq!(
            out.status.code(),
            Some(1),
            "{resource}: {}",
            text(&out.stderr)
        );
        let records: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(records.as_array().unwrap().len(), 3);
        for (index, target) in ["first", "missing", "last"].into_iter().enumerate() {
            let record = &records[index];
            assert_eq!(record["target"], target);
            assert_eq!(record["success"], index != 1);
            if index == 1 {
                assert!(record["id"].is_null());
                assert_eq!(record["error"]["origin"], "operation");
                assert_eq!(record["error"]["target"], "missing");
                assert_eq!(record["error"]["kind"], "NotFound");
                assert!(
                    record["error"]["message"]
                        .as_str()
                        .unwrap()
                        .contains("missing")
                );
            } else {
                assert_eq!(record["id"], target);
                assert!(record["error"].is_null());
            }
        }
        let diagnostic: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert_eq!(diagnostic, records[1]["error"]);
        let inspected = v(&[resource, "inspect", "first", "last"]);
        assert_eq!(inspected.status.code(), Some(1));
        assert_eq!(
            serde_json::from_slice::<Value>(&inspected.stdout).unwrap(),
            json!([])
        );
    }
}

#[test]
fn populated_machine_lists_use_public_typed_records() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let created = vmctl(
        dir.path(),
        &guestd,
        &[
            "machine", "create", "--name", "example", "--cpus", "2", "--memory", "128M", "img",
            "true",
        ],
    );
    assert!(created.status.success(), "{}", text(&created.stderr));
    let id = text(&created.stdout).trim().to_owned();
    for args in [
        vec!["machine", "ls", "--all", "--output", "json"],
        vec!["ps", "--all", "--output", "json"],
    ] {
        let out = vmctl(dir.path(), &guestd, &args);
        assert!(out.status.success());
        let records: Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(records.as_array().unwrap().len(), 1);
        let r = &records[0];
        assert_eq!(r["id"], id);
        assert_eq!(r["name"], "example");
        assert_eq!(r["host_config"]["cpus"], 2);
        assert_eq!(r["host_config"]["memory"], 134217728);
        assert_eq!(r["config"]["open_stdin"], false);
        assert_eq!(r["state"]["status"], "created");
        assert_eq!(r["state"]["oom_killed"], false);
        assert!(r["state"]["started_at"].is_null());
        assert!(r["created"].as_u64().unwrap() > 1_000_000_000_000);
        assert!(out.stderr.is_empty());
    }
}

#[test]
fn populated_volume_lists_preserve_numeric_sizes_and_timestamp_types() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    assert!(
        vmctl(
            dir.path(),
            &guestd,
            &["volume", "create", "--size", "4M", "example"]
        )
        .status
        .success()
    );
    let out = vmctl(dir.path(), &guestd, &["volume", "ls", "--output", "json"]);
    assert!(out.status.success());
    let records: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(records.as_array().unwrap().len(), 1);
    assert_eq!(records[0]["name"], "example");
    assert_eq!(records[0]["size"], 4194304);
    assert!(records[0]["path"].is_string());
    assert!(records[0]["created"].as_u64().unwrap() > 1_000_000_000_000);
    assert!(out.stderr.is_empty());
}
