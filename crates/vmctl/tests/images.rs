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
    assert!(text(&empty.stdout).starts_with("REPOSITORY"));
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
    let fields: Vec<_> = row.split_whitespace().collect();
    assert_eq!(fields[0], format!("{}/test/app", reg.addr));
    assert_eq!(fields[1], "v1");
    assert_eq!(fields[2], &digest["sha256:".len().."sha256:".len() + 12]);
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
    assert_eq!(out.status.code(), Some(125));
    assert!(text(&out.stderr).contains("nope"), "{}", text(&out.stderr));
}
