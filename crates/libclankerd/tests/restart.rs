//! Seam A: restart policies restart (or do not restart) the workload as Docker would.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::*;
use libclankerd::{EventAction, HostConfig, Machine, RestartPolicy, Status};

/// Appends a line to `log` on every run, then exits with `code`.
fn counting(log: &Path, code: i32) -> String {
    format!("echo run >> {}; exit {code}", log.display())
}

fn runs(log: &Path) -> usize {
    std::fs::read_to_string(log).map_or(0, |s| s.lines().count())
}

fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !ok() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn create_with(env: &Env, name: &str, script: &str, policy: RestartPolicy) -> Machine {
    env.engine()
        .create(
            Some(name),
            sh(script),
            HostConfig {
                restart_policy: policy,
                ..Default::default()
            },
        )
        .unwrap()
}

#[test]
fn policies_parse_like_docker_run_restart() {
    assert_eq!("no".parse(), Ok(RestartPolicy::No));
    assert_eq!("always".parse(), Ok(RestartPolicy::Always));
    assert_eq!("unless-stopped".parse(), Ok(RestartPolicy::UnlessStopped));
    assert_eq!(
        "on-failure".parse(),
        Ok(RestartPolicy::OnFailure { max_retries: None })
    );
    assert_eq!(
        "on-failure:3".parse(),
        Ok(RestartPolicy::OnFailure {
            max_retries: Some(3)
        })
    );
    assert!("sometimes".parse::<RestartPolicy>().is_err());
    assert!("on-failure:x".parse::<RestartPolicy>().is_err());
    assert_eq!(
        RestartPolicy::OnFailure {
            max_retries: Some(3)
        }
        .to_string(),
        "on-failure:3"
    );
}

#[test]
fn no_policy_does_not_restart() {
    let env = Env::new();
    let log = env.root().join("log");
    let m = create_with(&env, "never", &counting(&log, 1), RestartPolicy::No);
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 1);
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(runs(&log), 1);
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
}

#[test]
fn on_failure_retries_up_to_the_limit_then_stays_exited() {
    let env = Env::new();
    let log = env.root().join("log");
    let m = create_with(
        &env,
        "flaky",
        &counting(&log, 1),
        RestartPolicy::OnFailure {
            max_retries: Some(2),
        },
    );
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 1);
    let state = m.inspect().unwrap().state;
    assert_eq!(state.status, Status::Exited);
    assert_eq!(state.restart_count, 2);
    assert_eq!(runs(&log), 3, "one run plus two restarts");
}

#[test]
fn on_failure_does_not_restart_a_clean_exit() {
    let env = Env::new();
    let log = env.root().join("log");
    let m = create_with(
        &env,
        "clean",
        &counting(&log, 0),
        RestartPolicy::OnFailure { max_retries: None },
    );
    m.start().unwrap();
    m.wait().unwrap();
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(runs(&log), 1);
}

#[test]
fn always_restarts_even_a_clean_exit_until_stopped() {
    let env = Env::new();
    let log = env.root().join("log");
    let m = create_with(&env, "again", &counting(&log, 0), RestartPolicy::Always);
    m.start().unwrap();
    wait_until("three runs", || runs(&log) >= 3);

    m.stop(Duration::from_secs(10)).unwrap();
    let state = m.inspect().unwrap().state;
    assert_eq!(state.status, Status::Exited);
    let seen = runs(&log);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(runs(&log), seen, "a stopped machine is not restarted");
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
}

#[test]
fn unless_stopped_restarts_until_stopped() {
    let env = Env::new();
    let log = env.root().join("log");
    let m = create_with(&env, "us", &counting(&log, 2), RestartPolicy::UnlessStopped);
    m.start().unwrap();
    wait_until("two runs", || runs(&log) >= 2);
    m.stop(Duration::from_secs(10)).unwrap();
    let seen = runs(&log);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(runs(&log), seen);
}

#[test]
fn a_restarting_machine_reports_restarting_and_emits_events() {
    let env = Env::new();
    let engine = env.engine();
    let log = env.root().join("log");
    let m = engine
        .create(
            Some("evt"),
            sh(&counting(&log, 1)),
            HostConfig {
                restart_policy: RestartPolicy::OnFailure {
                    max_retries: Some(1),
                },
                ..Default::default()
            },
        )
        .unwrap();
    let mut events = engine.events();
    m.start().unwrap();
    m.wait().unwrap();

    let mut actions = Vec::new();
    while let Some(e) = events.next_timeout(Duration::from_millis(300)) {
        actions.push(e.action);
    }
    use EventAction::*;
    assert_eq!(
        actions,
        [Started, Exited, Restarting, Started, Exited],
        "events in order"
    );
}

#[test]
fn removing_a_machine_during_the_restart_delay_cancels_the_restart() {
    let env = Env::new();
    let log = env.root().join("log");
    let m = create_with(&env, "cancel", &counting(&log, 1), RestartPolicy::Always);
    m.start().unwrap();
    wait_until("one run", || runs(&log) >= 1);
    m.remove(true).unwrap();
    let seen = runs(&log);
    std::thread::sleep(Duration::from_millis(700));
    assert_eq!(runs(&log), seen);
}
