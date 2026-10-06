//! Seam A: running a command in a machine, through the public library API only.

mod common;

use std::os::unix::net::UnixListener;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use libclankerd::vmm::{BootHandle, BootSpec};
use libclankerd::{Engine, ErrorKind, HostConfig, MachineConfig, Status, Vmm};

fn wait_script(marker: &std::path::Path, exit: i32) -> String {
    format!(
        "while [ ! -e {} ]; do sleep 0.05; done; exit {exit}",
        marker.display()
    )
}

#[test]
fn run_returns_exit_code_zero() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "ok", "exit 0");
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 0);
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
}

#[test]
fn run_returns_nonzero_exit_code() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "three", "exit 3");
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 3);
    let state = m.inspect().unwrap().state;
    assert_eq!(state.status, Status::Exited);
    assert_eq!(state.exit_code, 3);
}

#[test]
fn killed_workload_reports_128_plus_signal() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "sig", "kill -9 $$");
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 137);
}

#[test]
fn missing_binary_exits_127_like_docker() {
    let env = Env::new();
    let engine = env.engine();
    let cfg = MachineConfig {
        image: "placeholder".into(),
        cmd: vec!["/no/such/binary".into()],
        ..Default::default()
    };
    let m = engine.create(None, cfg, HostConfig::default()).unwrap();
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 127);
}

#[test]
fn env_and_working_dir_reach_the_workload() {
    let env = Env::new();
    let engine = env.engine();
    let out = env.root().join("out");
    let mut cfg = sh(&format!(
        "echo \"$GREETING\" > {o}; pwd >> {o}",
        o = out.display()
    ));
    cfg.env = vec!["GREETING=hello".into()];
    cfg.working_dir = "/tmp".into();
    let m = engine
        .create(Some("env"), cfg, HostConfig::default())
        .unwrap();
    m.start().unwrap();
    m.wait().unwrap();
    assert_eq!(std::fs::read_to_string(out).unwrap(), "hello\n/tmp\n");
}

#[test]
fn state_goes_created_running_exited() {
    let env = Env::new();
    let engine = env.engine();
    let marker = env.root().join("go");
    let m = create(&engine, "life", &wait_script(&marker, 2));
    assert_eq!(m.inspect().unwrap().state.status, Status::Created);

    m.start().unwrap();
    // start() returns only after guestd's ready event: the machine is running.
    let state = m.inspect().unwrap().state;
    assert_eq!(state.status, Status::Running);
    assert!(state.started_at.is_some());
    assert_eq!(engine.list(false).unwrap().len(), 1);

    std::fs::write(&marker, "").unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 2);
    let state = m.inspect().unwrap().state;
    assert_eq!(state.status, Status::Exited);
    assert!(state.finished_at.is_some());
    assert!(engine.list(false).unwrap().is_empty());
    assert_eq!(engine.list(true).unwrap().len(), 1);
}

#[test]
fn remove_deletes_state() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "gone", "exit 0");
    m.start().unwrap();
    m.wait().unwrap();
    m.remove(false).unwrap();
    assert_eq!(
        engine.get("gone").err().unwrap().kind(),
        ErrorKind::NotFound
    );
    assert!(engine.list(true).unwrap().is_empty());
    assert_eq!(m.inspect().unwrap_err().kind(), ErrorKind::NotFound);
}

#[test]
fn running_machine_needs_force_to_remove() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "busy", "sleep 60");
    m.start().unwrap();
    assert_eq!(m.remove(false).unwrap_err().kind(), ErrorKind::Conflict);
    m.remove(true).unwrap();
    assert!(engine.list(true).unwrap().is_empty());
}

#[test]
fn config_and_state_survive_a_restart() {
    let env = Env::new();
    {
        let engine = env.engine();
        let m = create(&engine, "persist", "exit 6");
        m.start().unwrap();
        m.wait().unwrap();
    }
    let engine = env.engine();
    let info = engine.get("persist").unwrap().inspect().unwrap();
    assert_eq!(info.config.cmd, vec!["exit 6".to_string()]);
    assert_eq!(info.state.status, Status::Exited);
    assert_eq!(info.state.exit_code, 6);
}

#[test]
fn a_running_machine_is_reattached_after_restart() {
    let env = Env::new();
    let marker = env.root().join("go");
    let first = env.engine();
    create(&first, "survivor", &wait_script(&marker, 4))
        .start()
        .unwrap();

    // A second Engine over the same directories stands in for a restarted process.
    let second = env.engine();
    let m = second.get("survivor").unwrap();
    assert_eq!(m.inspect().unwrap().state.status, Status::Running);
    std::fs::write(&marker, "").unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 4);
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
}

#[test]
fn exit_while_the_library_was_away_is_recorded_on_reattach() {
    let env = Env::new();
    let first = env.engine();
    let m = create(&first, "away", "sleep 0.3; exit 9");
    m.start().unwrap();
    // Wait for the exit, then rewind the persisted state as if no process had seen it.
    assert_eq!(m.wait().unwrap().exit_code, 9);
    let path = env
        .root()
        .join("state/machines")
        .join(m.id())
        .join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["status"] = "running".into();
    state["exit_code"] = 0.into();
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

    let second = env.engine();
    let info = second.get("away").unwrap().inspect().unwrap();
    assert_eq!(info.state.status, Status::Exited);
    assert_eq!(info.state.exit_code, 9);
}

#[test]
fn start_times_out_with_a_clear_error_if_guest_never_reports_ready() {
    struct Mute;
    impl Vmm for Mute {
        fn boot(&self, spec: &BootSpec) -> libclankerd::Result<BootHandle> {
            // Listens, never answers: a guest that hangs before ready.
            let l = UnixListener::bind(&spec.guest_socket).unwrap();
            std::mem::forget(l);
            Ok(BootHandle { pid: 0x7fff_fff0 })
        }
    }
    let env = Env::new();
    let mut cfg = env.config(Arc::new(Mute));
    cfg.start_timeout = Duration::from_millis(300);
    let engine = Engine::new(cfg).unwrap();
    let m = create(&engine, "mute", "true");
    let err = m.start().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(
        err.message().contains("timed out") && err.message().contains("ready"),
        "{err}"
    );
    let state = m.inspect().unwrap().state;
    assert_eq!(state.status, Status::Created);
    assert!(state.error.contains("timed out"));
}

#[test]
fn vmm_boot_failure_is_unavailable() {
    let env = Env::new();
    let engine = Engine::new(env.config(Arc::new(libclankerd::vmm::UnavailableVmm))).unwrap();
    let m = create(&engine, "nobackend", "true");
    assert_eq!(m.start().unwrap_err().kind(), ErrorKind::Unavailable);
}

#[test]
fn errors_use_docker_kinds() {
    let env = Env::new();
    let engine = env.engine();
    create(&engine, "dup", "true");

    let conflict = engine.create(Some("dup"), sh("true"), HostConfig::default());
    assert_eq!(conflict.err().unwrap().kind(), ErrorKind::Conflict);
    assert_eq!(
        engine.get("nope").err().unwrap().kind(),
        ErrorKind::NotFound
    );

    let bad_name = engine.create(Some("bad name!"), sh("true"), HostConfig::default());
    assert_eq!(bad_name.err().unwrap().kind(), ErrorKind::InvalidParameter);
    let no_image = MachineConfig {
        cmd: vec!["x".into()],
        ..Default::default()
    };
    let no_image = engine.create(None, no_image, HostConfig::default());
    assert_eq!(no_image.err().unwrap().kind(), ErrorKind::InvalidParameter);
    let no_cmd = MachineConfig {
        image: "i".into(),
        ..Default::default()
    };
    let no_cmd = engine.create(None, no_cmd, HostConfig::default());
    assert_eq!(no_cmd.err().unwrap().kind(), ErrorKind::InvalidParameter);

    let m = create(&engine, "twice", "sleep 60");
    m.start().unwrap();
    assert_eq!(m.start().unwrap_err().kind(), ErrorKind::Conflict);
    m.remove(true).unwrap();
}

#[test]
fn machines_are_found_by_unique_id_prefix() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "pref", "true");
    let found = engine.get(&m.id()[..12]).unwrap();
    assert_eq!(found.id(), m.id());
}
