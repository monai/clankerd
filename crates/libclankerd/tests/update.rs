//! Seam A: `Machine::update` changes a stopped machine's resources, mounts
//! and socket bindings, and refuses the rest with the list of immutable fields.

mod common;

use common::*;
use libclankerd::{ErrorKind, EventAction, HostConfig, Mount};
use std::time::Duration;

#[test]
fn update_applies_resources_and_mounts_to_a_stopped_machine() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "upd", "exit 0");
    let info = m.inspect().unwrap();
    let mut events = engine.events();

    let host = HostConfig {
        cpus: Some(4),
        memory: Some(2 << 30),
        mounts: vec![Mount::volume("data", "/storage")],
        ..info.host_config.clone()
    };
    m.update(&info.config, host.clone()).unwrap();

    let after = m.inspect().unwrap();
    assert_eq!(after.host_config, host);
    assert_eq!(after.config, info.config);
    assert_eq!(engine.volume("data").unwrap().name, "data");
    let event = events.next_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(event.action, EventAction::Updated);

    // The new configuration is what the next start uses.
    drop(engine);
    let engine = env.engine();
    assert_eq!(
        engine.get("upd").unwrap().inspect().unwrap().host_config,
        host
    );
}

#[test]
fn update_rejects_immutable_fields_and_lists_them() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "fixed", "exit 0");
    let info = m.inspect().unwrap();

    let mut config = info.config.clone();
    config.image = "other:latest".into();
    config.cmd = vec!["true".into()];
    let host = HostConfig {
        cpus: Some(8),
        ..Default::default()
    };
    let err = m.update(&config, host).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert!(err.message().contains("image"), "{err}");
    assert!(err.message().contains("cmd"), "{err}");
    assert!(!err.message().contains("entrypoint"), "{err}");
    // Nothing was applied.
    assert_eq!(m.inspect().unwrap().host_config, info.host_config);
}

#[test]
fn update_refuses_a_running_machine() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "live", "sleep 60");
    m.start().unwrap();
    let info = m.inspect().unwrap();
    let err = m.update(&info.config, HostConfig::default()).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);
    m.remove(true).unwrap();
}

#[test]
fn update_validates_the_new_host_config() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "bad", "exit 0");
    let info = m.inspect().unwrap();
    let host = HostConfig {
        socket_bindings: vec![libclankerd::SocketBinding {
            guest_path: "relative.sock".into(),
            host_path: "/host.sock".into(),
        }],
        ..Default::default()
    };
    let err = m.update(&info.config, host).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
}
