//! Seam A: changing a machine's image rebuilds its root disk and keeps its volume.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::registry::{Registry, layer};
use common::{Env, TarCapture, guestd_path};
use libclankerd::vmm::LocalProcessVmm;
use libclankerd::{
    Engine, ErrorKind, EventAction, HostConfig, MachineConfig, Mount, PullPolicy, ROOT_DISK,
};
use serde_json::json;

fn setup() -> (Env, Registry, Engine) {
    let env = Env::new();
    let registry = Registry::start();
    let mut cfg = env.config(Arc::new(LocalProcessVmm::new(guestd_path())));
    cfg.cache_dir = Some(env.root().join("cache"));
    cfg.populator = Some(Arc::new(TarCapture::default()));
    cfg.insecure_registries = vec![registry.addr.clone()];
    let engine = Engine::new(cfg).unwrap();
    (env, registry, engine)
}

fn disk_of(env: &Env, id: &str) -> std::path::PathBuf {
    env.root().join("state/machines").join(id).join(ROOT_DISK)
}

fn tar_has(disk: &Path, name: &str) -> bool {
    let mut ar = tar::Archive::new(std::fs::File::open(disk).unwrap());
    ar.entries().unwrap().any(|e| {
        e.unwrap()
            .path()
            .unwrap()
            .to_string_lossy()
            .trim_end_matches('/')
            == name
    })
}

#[test]
fn changing_the_image_rebuilds_the_root_disk_and_keeps_the_volume() {
    let (env, registry, engine) = setup();
    let v1 = registry.push_with_config(
        "t/app",
        "v1",
        &[layer(&[("only-in-v1", b"1")])],
        json!({"Cmd": ["echo", "one"], "User": "agent"}),
    );
    let v2 = registry.push_with_config(
        "t/app",
        "v2",
        &[layer(&[("only-in-v2", b"2")])],
        json!({"Cmd": ["echo", "two"], "User": "other"}),
    );
    let (r1, r2) = (
        format!("{}/t/app:v1", registry.addr),
        format!("{}/t/app:v2", registry.addr),
    );
    let m = engine
        .create(
            Some("up"),
            MachineConfig {
                image: r1,
                env: vec!["MINE=1".into()],
                ..Default::default()
            },
            HostConfig {
                mounts: vec![Mount::volume("data", "/storage")],
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(m.inspect().unwrap().image_id, v1);
    let volume = engine.volume("data").unwrap().path;
    std::fs::write(&volume, b"precious").unwrap();
    let mut events = engine.events();

    m.change_image(&r2).unwrap();

    let info = m.inspect().unwrap();
    assert_eq!(info.image_id, v2);
    assert_eq!(info.config.image, r2);
    assert_eq!(info.config.cmd, ["echo", "two"], "the new image's command");
    assert_eq!(info.config.user, "other");
    assert!(
        info.config.env.contains(&"MINE=1".to_string()),
        "overrides kept"
    );
    let disk = disk_of(&env, m.id());
    assert!(tar_has(&disk, "only-in-v2") && !tar_has(&disk, "only-in-v1"));
    assert_eq!(&std::fs::read(&volume).unwrap()[..8], b"precious");
    assert_eq!(
        events.next_timeout(Duration::from_secs(5)).unwrap().action,
        EventAction::Updated
    );
}

#[test]
fn changing_the_image_of_a_running_machine_is_a_conflict() {
    let (_env, registry, engine) = setup();
    registry.push("t/app", "v1", &[layer(&[("f", b"1")])]);
    registry.push("t/app", "v2", &[layer(&[("f", b"2")])]);
    let m = engine
        .create(
            Some("run"),
            MachineConfig {
                image: format!("{}/t/app:v1", registry.addr),
                entrypoint: vec!["/bin/sh".into(), "-c".into()],
                cmd: vec!["sleep 60".into()],
                ..Default::default()
            },
            HostConfig::default(),
        )
        .unwrap();
    m.start().unwrap();
    let err = m
        .change_image(&format!("{}/t/app:v2", registry.addr))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    m.remove(true).unwrap();
}

#[test]
fn change_image_follows_the_pull_policy() {
    let (_env, registry, engine) = setup();
    registry.push("t/app", "v1", &[layer(&[("f", b"1")])]);
    registry.push("t/app", "v2", &[layer(&[("f", b"2")])]);
    engine.pull(&format!("{}/t/app:v1", registry.addr)).unwrap();
    let m = engine
        .create(
            Some("never"),
            MachineConfig {
                image: format!("{}/t/app:v1", registry.addr),
                cmd: vec!["true".into()],
                ..Default::default()
            },
            HostConfig {
                pull_policy: PullPolicy::Never,
                ..Default::default()
            },
        )
        .unwrap();
    // v2 was never pulled and the policy forbids it.
    let err = m
        .change_image(&format!("{}/t/app:v2", registry.addr))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(m.inspect().unwrap().config.image.ends_with(":v1"));
}
