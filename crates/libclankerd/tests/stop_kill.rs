//! Seam A: stopping and killing machines. Graceful stop goes through guestd
//! (the workload is signalled and the machine ends when it does); a workload
//! that ignores the signal is killed after the timeout.

mod common;

use std::time::{Duration, Instant};

use common::*;
use libclankerd::{ErrorKind, Status};

/// Waits until `path` exists (the workload says it is ready).
fn wait_for(path: &std::path::Path) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "{} never appeared",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn stop_lets_the_workload_handle_sigterm_and_exit_cleanly() {
    let env = Env::new();
    let engine = env.engine();
    let (ready, bye) = (env.root().join("ready"), env.root().join("bye"));
    let m = create(
        &engine,
        "graceful",
        &format!(
            "trap 'echo flushed > {bye}; exit 0' TERM; touch {ready}; while true; do sleep 0.05; done",
            bye = bye.display(),
            ready = ready.display()
        ),
    );
    m.start().unwrap();
    wait_for(&ready);

    let began = Instant::now();
    m.stop(Duration::from_secs(10)).unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "stop was not prompt"
    );

    assert_eq!(std::fs::read_to_string(&bye).unwrap(), "flushed\n");
    let state = m.inspect().unwrap().state;
    assert_eq!(state.status, Status::Exited);
    assert_eq!(state.exit_code, 0);
}

#[test]
fn stop_reports_128_plus_sigterm_for_a_workload_that_does_not_handle_it() {
    let env = Env::new();
    let engine = env.engine();
    let ready = env.root().join("ready");
    let m = create(
        &engine,
        "plain",
        &format!("touch {}; sleep 60", ready.display()),
    );
    m.start().unwrap();
    wait_for(&ready);
    m.stop(Duration::from_secs(10)).unwrap();
    let state = m.inspect().unwrap().state;
    assert_eq!(state.status, Status::Exited);
    assert_eq!(state.exit_code, 143);
}

#[test]
fn stop_kills_a_workload_that_ignores_sigterm_after_the_timeout() {
    let env = Env::new();
    let engine = env.engine();
    let ready = env.root().join("ready");
    let m = create(
        &engine,
        "stubborn",
        &format!(
            "trap '' TERM; touch {}; while true; do sleep 0.05; done",
            ready.display()
        ),
    );
    m.start().unwrap();
    wait_for(&ready);
    let pid = m.inspect().unwrap().state.pid.unwrap();

    let began = Instant::now();
    m.stop(Duration::from_millis(600)).unwrap();
    let took = began.elapsed();
    assert!(
        took >= Duration::from_millis(500),
        "gave up early: {took:?}"
    );
    assert!(
        took < Duration::from_secs(8),
        "kill fallback too slow: {took:?}"
    );

    let state = m.inspect().unwrap().state;
    assert_eq!(state.status, Status::Exited);
    assert_eq!(state.exit_code, 137);
    // The helper process is gone.
    // SAFETY: signal 0 only probes for existence.
    assert_ne!(unsafe { libc::kill(pid as i32, 0) }, 0);
}

#[test]
fn kill_ends_the_machine_immediately_with_137() {
    let env = Env::new();
    let engine = env.engine();
    let ready = env.root().join("ready");
    let m = create(
        &engine,
        "doomed",
        &format!("trap '' TERM; touch {}; sleep 60", ready.display()),
    );
    m.start().unwrap();
    wait_for(&ready);
    m.kill(libc::SIGKILL).unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 137);
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
}

#[test]
fn kill_with_another_signal_goes_to_the_workload_only() {
    let env = Env::new();
    let engine = env.engine();
    let ready = env.root().join("ready");
    let m = create(
        &engine,
        "usr1",
        &format!(
            "trap 'exit 5' USR1; touch {}; while true; do sleep 0.05; done",
            ready.display()
        ),
    );
    m.start().unwrap();
    wait_for(&ready);
    m.kill(libc::SIGUSR1).unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 5);
}

#[test]
fn stopping_a_stopped_machine_is_fine_and_killing_it_is_a_conflict() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "idle", "true");
    m.stop(Duration::from_secs(1)).unwrap(); // never started
    m.start().unwrap();
    m.wait().unwrap();
    m.stop(Duration::from_secs(1)).unwrap(); // already exited
    assert_eq!(
        m.kill(libc::SIGKILL).unwrap_err().kind(),
        ErrorKind::Conflict
    );
}

#[test]
fn a_stopped_machine_can_be_started_again() {
    let env = Env::new();
    let engine = env.engine();
    let ready = env.root().join("ready");
    let m = create(
        &engine,
        "again",
        &format!("touch {}; sleep 60", ready.display()),
    );
    m.start().unwrap();
    wait_for(&ready);
    m.stop(Duration::from_secs(10)).unwrap();
    std::fs::remove_file(&ready).unwrap();
    m.start().unwrap();
    wait_for(&ready);
    assert_eq!(m.inspect().unwrap().state.status, Status::Running);
    m.kill(libc::SIGKILL).unwrap();
    m.wait().unwrap();
}

#[test]
fn stop_sends_the_configured_stop_signal_instead_of_sigterm() {
    let env = Env::new();
    let engine = env.engine();
    let (ready, bye) = (env.root().join("ready"), env.root().join("bye"));
    let script = format!(
        "trap 'echo usr1 > {bye}; exit 0' USR1; trap 'exit 9' TERM; touch {ready}; while true; do sleep 0.05; done",
        bye = bye.display(),
        ready = ready.display()
    );
    let m = engine
        .create(
            Some("quit-signal"),
            libclankerd::MachineConfig {
                stop_signal: "SIGUSR1".into(),
                ..sh(&script)
            },
            libclankerd::HostConfig::default(),
        )
        .unwrap();
    m.start().unwrap();
    wait_for(&ready);
    m.stop(Duration::from_secs(10)).unwrap();
    assert_eq!(std::fs::read_to_string(&bye).unwrap(), "usr1\n");
    assert_eq!(m.inspect().unwrap().state.exit_code, 0);
}

#[test]
fn an_unknown_stop_signal_is_refused_at_create() {
    let env = Env::new();
    let engine = env.engine();
    let err = engine
        .create(
            None,
            libclankerd::MachineConfig {
                stop_signal: "SIGNOPE".into(),
                ..sh("true")
            },
            libclankerd::HostConfig::default(),
        )
        .err()
        .expect("refused");
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);
}
