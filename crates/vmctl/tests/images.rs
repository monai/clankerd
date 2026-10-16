//! CLI: `vmctl pull` and `vmctl images`, against an in-process registry.

#[path = "../../libclankerd/tests/common/registry.rs"]
mod registry;

mod common;

use std::process::{Command, Output};

use common::text;

fn vmctl(dir: &std::path::Path, registry: &str, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .args(args)
        .env("CLANKERD_STATE_DIR", dir.join("state"))
        .env("CLANKERD_RUNTIME_DIR", dir.join("run"))
        .env("CLANKERD_CACHE_DIR", dir.join("cache"))
        .env("CLANKERD_INSECURE_REGISTRIES", registry)
        .output()
        .unwrap()
}

#[test]
fn pull_then_images_lists_repository_tag_and_id() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let reg = registry::Registry::start();
    let digest = reg.push("test/app", "v1", &[registry::layer(&[("hello", b"hi")])]);
    let reference = format!("{}/test/app:v1", reg.addr);
    let v = |args: &[&str]| vmctl(dir.path(), &reg.addr, args);

    let empty = v(&["images"]);
    assert!(empty.status.success());
    assert!(text(&empty.stdout).starts_with("IMAGE ID"));
    assert_eq!(text(&empty.stdout).lines().count(), 1);

    let pull = v(&["pull", &reference]);
    assert!(pull.status.success(), "{}", text(&pull.stderr));
    assert!(
        text(&pull.stdout).contains(&digest),
        "{}",
        text(&pull.stdout)
    );

    let images = v(&["images"]);
    let out = text(&images.stdout);
    let row = out.lines().nth(1).expect("one image row");
    assert!(row.contains(&digest));
    assert!(row.contains(&reference));
}

#[test]
fn pulling_a_missing_image_fails_with_a_clear_error() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let reg = registry::Registry::start();
    let out = vmctl(
        dir.path(),
        &reg.addr,
        &["pull", &format!("{}/test/nope:v1", reg.addr)],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("nope"), "{}", text(&out.stderr));
}

#[test]
fn json_image_pulls_return_typed_image_objects() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry::Registry::start();
    let layer = registry::layer(&[("example", b"content")]);
    let size = layer.len() as u64;
    let digest = reg.push("example/app", "v1", &[layer]);
    let reference = format!("{}/example/app:v1", reg.addr);
    for prefix in [vec!["image", "pull"], vec!["pull"]] {
        let mut args = prefix;
        args.extend(["--output", "json", &reference]);
        let out = vmctl(dir.path(), &reg.addr, &args);
        assert!(out.status.success(), "{}", text(&out.stderr));
        assert!(out.stderr.is_empty());
        let record: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert!(record.is_object());
        assert_eq!(record["id"], digest);
        assert_eq!(record["size"], size);
        assert_eq!(record["layers"][0]["size"], size);
        assert_eq!(record["layers"].as_array().unwrap().len(), 1);
        assert!(
            record["references"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!(reference))
        );
        assert!(record["pulled"].as_u64().unwrap() > 1_000_000_000_000);
    }
}

#[test]
fn populated_image_lists_return_typed_image_arrays() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry::Registry::start();
    let layer = registry::layer(&[("example", b"content")]);
    let size = layer.len() as u64;
    let digest = reg.push("example/app", "v1", &[layer]);
    let reference = format!("{}/example/app:v1", reg.addr);
    assert!(
        vmctl(dir.path(), &reg.addr, &["image", "pull", &reference])
            .status
            .success()
    );
    for prefix in [vec!["image", "ls"], vec!["images"]] {
        let mut args = prefix;
        args.extend(["--output", "json"]);
        let out = vmctl(dir.path(), &reg.addr, &args);
        assert!(out.status.success());
        assert!(out.stderr.is_empty());
        let records: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(records.as_array().unwrap().len(), 1);
        assert_eq!(records[0]["id"], digest);
        assert_eq!(records[0]["size"], size);
    }
}

#[test]
fn failed_json_pulls_emit_null_and_an_operation_diagnostic() {
    let dir = tempfile::tempdir().unwrap();
    let reg = registry::Registry::start();
    for prefix in [vec!["image", "pull"], vec!["pull"]] {
        let reference = format!("{}/example/missing:v1", reg.addr);
        let mut args = prefix;
        args.extend(["--output", "json", &reference]);
        let out = vmctl(dir.path(), &reg.addr, &args);
        assert_eq!(out.status.code(), Some(1));
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap(),
            serde_json::Value::Null
        );
        let error: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
        assert_eq!(error["origin"], "operation");
        assert!(error["message"].as_str().unwrap().contains("missing"));
    }
}
