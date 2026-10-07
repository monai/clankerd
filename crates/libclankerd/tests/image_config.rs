//! Seam A: the image's runtime config (ENTRYPOINT, CMD, Env, User, WorkingDir)
//! is read at create time, merged with the developer's overrides the way
//! Docker does, stored with the machine and what the workload then runs with.

mod common;

use std::sync::Arc;

use common::registry::{Registry, layer};
use common::{Env, TarCapture, guestd_path};
use libclankerd::vmm::LocalProcessVmm;
use libclankerd::{Engine, ErrorKind, HostConfig, Machine, MachineConfig};

struct Fixture {
    env: Env,
    registry: Registry,
    engine: Engine,
}

fn fixture() -> Fixture {
    let env = Env::new();
    let registry = Registry::start();
    let mut cfg = env.config(Arc::new(LocalProcessVmm::new(guestd_path())));
    cfg.cache_dir = Some(env.root().join("cache"));
    cfg.populator = Some(Arc::new(TarCapture::default()));
    cfg.insecure_registries = vec![registry.addr.clone()];
    let engine = Engine::new(cfg).unwrap();
    Fixture {
        env,
        registry,
        engine,
    }
}

impl Fixture {
    fn push(&self, tag: &str, runtime: serde_json::Value) -> String {
        self.registry
            .push_with_config("test/app", tag, &[layer(&[("hello", b"hi")])], runtime);
        format!("{}/test/app:{tag}", self.registry.addr)
    }

    fn create(&self, image: &str, overrides: MachineConfig) -> libclankerd::Result<Machine> {
        self.engine.create(
            None,
            MachineConfig {
                image: image.into(),
                ..overrides
            },
            HostConfig::default(),
        )
    }
}

#[test]
fn inspect_reports_the_image_defaults_merged_with_overrides_and_stores_the_image_config() {
    let f = fixture();
    let image = f.push(
        "v1",
        serde_json::json!({
            "Entrypoint": ["/bin/sh", "-c"],
            "Cmd": ["echo default"],
            "Env": ["PATH=/usr/bin", "MODE=image"],
            "User": "agent",
            "WorkingDir": "/srv",
        }),
    );

    let plain = f.create(&image, MachineConfig::default()).unwrap();
    let info = plain.inspect().unwrap();
    assert_eq!(info.config.entrypoint, ["/bin/sh", "-c"]);
    assert_eq!(info.config.cmd, ["echo default"]);
    assert_eq!(info.config.env, ["PATH=/usr/bin", "MODE=image"]);
    assert_eq!(info.config.user, "agent");
    assert_eq!(info.config.working_dir, "/srv");
    let stored = info.image_config.expect("the image config is stored");
    assert_eq!(stored.cmd, ["echo default"]);

    let overridden = f
        .create(
            &image,
            MachineConfig {
                cmd: vec!["echo".into(), "hi".into()],
                env: vec!["MODE=dev".into()],
                user: "root".into(),
                working_dir: "/tmp".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .inspect()
        .unwrap();
    assert_eq!(overridden.config.entrypoint, ["/bin/sh", "-c"]);
    assert_eq!(overridden.config.cmd, ["echo", "hi"]);
    assert_eq!(overridden.config.env, ["PATH=/usr/bin", "MODE=dev"]);
    assert_eq!(overridden.config.user, "root");
    assert_eq!(overridden.config.working_dir, "/tmp");
    // The stored defaults are the image's, not the merge.
    assert_eq!(overridden.image_config.unwrap().cmd, ["echo default"]);
}

#[test]
fn an_entrypoint_override_replaces_entrypoint_and_cmd() {
    let f = fixture();
    let image = f.push(
        "v1",
        serde_json::json!({"Entrypoint": ["/entry"], "Cmd": ["serve"]}),
    );
    let info = f
        .create(
            &image,
            MachineConfig {
                entrypoint: vec!["/bin/true".into()],
                ..Default::default()
            },
        )
        .unwrap()
        .inspect()
        .unwrap();
    assert_eq!(info.config.entrypoint, ["/bin/true"]);
    assert!(info.config.cmd.is_empty());
    assert_eq!(info.config.working_dir, "/");
}

#[test]
fn an_image_without_a_command_needs_one_from_the_caller() {
    let f = fixture();
    let image = f.push("bare", serde_json::json!({}));
    let err = f.create(&image, MachineConfig::default()).err().unwrap();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
    assert!(
        f.create(
            &image,
            MachineConfig {
                cmd: vec!["/bin/true".into()],
                ..Default::default()
            }
        )
        .is_ok()
    );
}

#[test]
fn the_workload_runs_the_merged_entrypoint_cmd_and_env() {
    let f = fixture();
    let out = f.env.root().join("out");
    let image = f.push(
        "v1",
        serde_json::json!({
            "Entrypoint": ["/bin/sh", "-c"],
            "Cmd": [format!("echo \"$A $B $PWD\" > {}", out.display())],
            "Env": ["A=1", "B=2"],
            "WorkingDir": "/tmp",
        }),
    );
    let m = f
        .create(
            &image,
            MachineConfig {
                env: vec!["B=3".into()],
                ..Default::default()
            },
        )
        .unwrap();
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 0);
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "1 3 /tmp\n");
}

#[test]
fn the_images_stopsignal_is_used_unless_overridden() {
    let f = fixture();
    let image = f.push(
        "sig",
        serde_json::json!({"Cmd": ["serve"], "StopSignal": "SIGQUIT"}),
    );
    let info = f
        .create(&image, MachineConfig::default())
        .unwrap()
        .inspect()
        .unwrap();
    assert_eq!(info.config.stop_signal, "SIGQUIT");

    let own = f
        .create(
            &image,
            MachineConfig {
                stop_signal: "SIGINT".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .inspect()
        .unwrap();
    assert_eq!(own.config.stop_signal, "SIGINT");
}
