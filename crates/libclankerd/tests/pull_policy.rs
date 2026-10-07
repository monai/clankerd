//! Seam A: `--pull missing|always|never` decides when create contacts the registry.

mod common;

use std::sync::Arc;

use common::registry::{Registry, layer};
use common::{Env, TarCapture, guestd_path, sh};
use libclankerd::vmm::LocalProcessVmm;
use libclankerd::{Engine, ErrorKind, HostConfig, PullPolicy};

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

fn create_with(engine: &Engine, reference: &str, pull: PullPolicy) -> libclankerd::Result<String> {
    let mut config = sh("true");
    config.image = reference.to_owned();
    let host = HostConfig {
        pull_policy: pull,
        ..Default::default()
    };
    engine
        .create(None, config, host)
        .map(|m| m.inspect().unwrap().image_id)
}

#[test]
fn policies_parse_like_docker_run_pull() {
    assert_eq!("missing".parse(), Ok(PullPolicy::Missing));
    assert_eq!("always".parse(), Ok(PullPolicy::Always));
    assert_eq!("never".parse(), Ok(PullPolicy::Never));
    assert!("sometimes".parse::<PullPolicy>().is_err());
}

#[test]
fn missing_pulls_only_what_is_not_cached() {
    let (_env, registry, engine) = setup();
    let first = registry.push("t/app", "v1", &[layer(&[("f", b"1")])]);
    let reference = format!("{}/t/app:v1", registry.addr);
    assert_eq!(
        create_with(&engine, &reference, PullPolicy::Missing).unwrap(),
        first
    );

    // The tag moves; the cached copy is still used.
    registry.push("t/app", "v1", &[layer(&[("f", b"2")])]);
    assert_eq!(
        create_with(&engine, &reference, PullPolicy::Missing).unwrap(),
        first
    );
}

#[test]
fn always_pulls_again_and_picks_up_a_moved_tag() {
    let (_env, registry, engine) = setup();
    let first = registry.push("t/app", "v1", &[layer(&[("f", b"1")])]);
    let reference = format!("{}/t/app:v1", registry.addr);
    assert_eq!(
        create_with(&engine, &reference, PullPolicy::Missing).unwrap(),
        first
    );

    let second = registry.push("t/app", "v1", &[layer(&[("f", b"2")])]);
    assert_ne!(first, second);
    assert_eq!(
        create_with(&engine, &reference, PullPolicy::Always).unwrap(),
        second
    );
}

#[test]
fn never_uses_the_cache_and_refuses_to_pull() {
    let (_env, registry, engine) = setup();
    registry.push("t/app", "v1", &[layer(&[("f", b"1")])]);
    let reference = format!("{}/t/app:v1", registry.addr);

    let err = create_with(&engine, &reference, PullPolicy::Never).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::NotFound);
    assert!(engine.images().unwrap().is_empty(), "nothing was pulled");

    engine.pull(&reference).unwrap();
    create_with(&engine, &reference, PullPolicy::Never).unwrap();
}
