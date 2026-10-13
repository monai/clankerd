//! Seam A: a restarted library picks machines up where it left them, and
//! several machines run side by side.

mod common;

use common::*;
use libclankerd::{HostConfig, RestartPolicy, Status};

#[test]
fn a_reattached_machine_keeps_its_restart_policy() {
    let env = Env::new();
    let go = env.root().join("go");
    let log = env.root().join("log");
    let script = format!(
        "echo run >> {log}; while [ ! -e {go} ]; do sleep 0.05; done; exit 1",
        log = log.display(),
        go = go.display()
    );
    let first = env.engine();
    let m = first
        .create(
            Some("kept"),
            sh(&script),
            HostConfig {
                restart_policy: RestartPolicy::OnFailure {
                    max_retries: Some(1),
                },
                ..Default::default()
            },
        )
        .unwrap();
    m.start().unwrap();

    let second = env.engine();
    let m = second.get("kept").unwrap();
    assert_eq!(m.inspect().unwrap().state.status, Status::Running);
    std::fs::write(&go, "").unwrap();

    // The workload fails once, is restarted by the policy, fails again.
    assert_eq!(m.wait().unwrap().exit_code, 1);
    let state = m.inspect().unwrap().state;
    assert_eq!((state.status, state.restart_count), (Status::Exited, 1));
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);
}

#[test]
fn a_machine_left_waiting_for_a_restart_is_restarted_by_the_new_process() {
    let env = Env::new();
    let log = env.root().join("log");
    let first = env.engine();
    let m = first
        .create(
            Some("waiting"),
            sh(&format!("echo run >> {}; sleep 60", log.display())),
            HostConfig {
                restart_policy: RestartPolicy::Always,
                ..Default::default()
            },
        )
        .unwrap();
    // The library died mid-delay: state says restarting, nothing is running.
    let path = env
        .root()
        .join("state/machines")
        .join(m.id())
        .join("state.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["status"] = "restarting".into();
    std::fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();

    let second = env.engine();
    let m = second.get("waiting").unwrap();
    wait_for("the restart", || {
        m.inspect().unwrap().state.status == Status::Running
    });
    assert!(log.exists());
    m.remove(true).unwrap();
}

#[test]
fn several_machines_run_side_by_side_independently() {
    let env = Env::new();
    let engine = env.engine();
    let machines: Vec<_> = (0..3)
        .map(|i| {
            let marker = env.root().join(format!("go{i}"));
            let m = create(
                &engine,
                &format!("m{i}"),
                &format!(
                    "while [ ! -e {} ]; do sleep 0.05; done; exit {}",
                    marker.display(),
                    10 + i
                ),
            );
            m.start().unwrap();
            m
        })
        .collect();
    assert_eq!(engine.list(false).unwrap().len(), 3);

    // Ending one leaves the others running.
    std::fs::write(env.root().join("go1"), "").unwrap();
    assert_eq!(machines[1].wait().unwrap().exit_code, 11);
    assert_eq!(engine.list(false).unwrap().len(), 2);
    assert_eq!(machines[0].inspect().unwrap().state.status, Status::Running);

    for i in [0, 2] {
        std::fs::write(env.root().join(format!("go{i}")), "").unwrap();
        assert_eq!(machines[i].wait().unwrap().exit_code, 10 + i as i32);
    }
}

#[test]
fn temporary_management_loss_does_not_end_a_live_machine_on_reattach() {
    let env = Env::new();
    let first = env.engine();
    let machine = create(&first, "management-gap", "sleep 60");
    machine.start().unwrap();
    let socket = env
        .root()
        .join("run")
        .join(format!("{}.sock", &machine.id()[..12]));
    let hidden = socket.with_extension("hidden");
    std::fs::rename(&socket, &hidden).unwrap();
    let second = env.engine();
    let status = second
        .get("management-gap")
        .unwrap()
        .inspect()
        .unwrap()
        .state
        .status;
    std::fs::rename(&hidden, &socket).unwrap();
    // Restore transport before cleanup even on the failing implementation.
    let third = env.engine();
    if status == Status::Running {
        let recovered = third.get("management-gap").unwrap();
        recovered.stop(std::time::Duration::from_secs(2)).unwrap();
        assert_eq!(recovered.inspect().unwrap().state.status, Status::Exited);
    } else {
        let state_path = env
            .root()
            .join("state/machines")
            .join(machine.id())
            .join("state.json");
        let mut state: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        state["status"] = "running".into();
        std::fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        let cleanup = env.engine();
        cleanup
            .get("management-gap")
            .unwrap()
            .stop(std::time::Duration::from_secs(2))
            .unwrap();
    }
    assert_eq!(
        status,
        Status::Running,
        "management loss must not be recorded as guest death"
    );
}

#[test]
fn a_vmm_that_really_died_is_recorded_dead_on_reattach() {
    let env = Env::new();
    let first = env.engine();
    let machine = create(&first, "vanished-vmm", "sleep 60");
    machine.start().unwrap();
    let state_path = env
        .root()
        .join("state/machines")
        .join(machine.id())
        .join("state.json");
    let persisted = std::fs::read(&state_path).unwrap();
    let state: serde_json::Value = serde_json::from_slice(&persisted).unwrap();
    let pid = state["pid"].as_u64().unwrap() as i32;
    // SAFETY: this is our test's live VMM process group; no other machine shares it.
    assert_eq!(unsafe { libc::kill(-pid, libc::SIGKILL) }, 0);
    wait_for("the actual VMM death", || {
        machine.inspect().unwrap().state.status == Status::Dead
    });
    // Stand in for a library that was away when the VMM disappeared.
    std::fs::write(&state_path, persisted).unwrap();
    let second = env.engine();
    assert_eq!(
        second
            .get("vanished-vmm")
            .unwrap()
            .inspect()
            .unwrap()
            .state
            .status,
        Status::Dead
    );
}
