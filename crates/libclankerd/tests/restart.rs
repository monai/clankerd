//! Restart policy governs machine failure, not a regular command's exit.
mod common;
use common::*;
use libclankerd::{HostConfig, RestartPolicy, Status};
use std::time::Duration;

#[test]
fn policies_parse_like_docker_run_restart() {
    for (text, policy) in [
        ("no", RestartPolicy::No),
        ("always", RestartPolicy::Always),
        ("unless-stopped", RestartPolicy::UnlessStopped),
        ("on-failure", RestartPolicy::OnFailure { max_retries: None }),
        (
            "on-failure:3",
            RestartPolicy::OnFailure {
                max_retries: Some(3),
            },
        ),
    ] {
        assert_eq!(text.parse(), Ok(policy));
        assert_eq!(policy.to_string(), text);
    }
    for text in ["sometimes", "on-failure:x"] {
        assert!(text.parse::<RestartPolicy>().is_err());
    }
}

#[test]
fn command_exit_does_not_restart_the_machine_under_any_policy() {
    let env = Env::new();
    let engine = env.engine();
    for (i, policy) in [
        RestartPolicy::No,
        RestartPolicy::Always,
        RestartPolicy::UnlessStopped,
        RestartPolicy::OnFailure {
            max_retries: Some(2),
        },
    ]
    .into_iter()
    .enumerate()
    {
        let machine = engine
            .create(
                Some(&format!("member-{i}")),
                sh("exit 7"),
                HostConfig {
                    restart_policy: policy,
                    ..Default::default()
                },
            )
            .unwrap();
        machine.start().unwrap();
        let pid = machine.inspect().unwrap().state.pid;
        assert_eq!(machine.wait_workload().unwrap().exit_code, 7);
        let state = machine.inspect().unwrap().state;
        assert_eq!(state.status, Status::Running);
        assert_eq!(state.pid, pid);
        assert_eq!(state.restart_count, 0);
        machine.stop(Duration::from_secs(5)).unwrap();
        assert_eq!(machine.inspect().unwrap().state.status, Status::Exited);
    }
}

#[test]
fn machine_failure_retries_to_the_limit_and_manual_stop_cancels_restart() {
    let env = Env::new();
    let engine = env.engine();
    let machine = engine
        .create(
            Some("member"),
            sh("sleep 60"),
            HostConfig {
                restart_policy: RestartPolicy::OnFailure {
                    max_retries: Some(2),
                },
                ..Default::default()
            },
        )
        .unwrap();
    machine.start().unwrap();
    for expected in 1..=2 {
        let old_pid = machine.inspect().unwrap().state.pid.unwrap();
        // A crashed VMM, independent of the guest command.
        // SAFETY: this PID identifies only the test-owned VMM process group.
        unsafe {
            libc::kill(-(old_pid as i32), libc::SIGKILL);
        }
        wait_for("replacement machine", || {
            let state = machine.inspect().unwrap().state;
            state.status == Status::Running
                && state.pid != Some(old_pid)
                && state.restart_count == expected
        });
    }
    let pid = machine.inspect().unwrap().state.pid.unwrap();
    // SAFETY: this PID identifies only the test-owned VMM process group.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    assert_eq!(machine.wait().unwrap().exit_code, -1);
    assert_eq!(machine.inspect().unwrap().state.restart_count, 2);
    machine.start().unwrap();
    machine.stop(Duration::from_secs(5)).unwrap();
    assert_eq!(machine.inspect().unwrap().state.status, Status::Exited);
}

#[test]
fn a_successful_command_cannot_mask_a_later_machine_failure() {
    let env = Env::new();
    let engine = env.engine();
    let machine = engine
        .create(
            Some("member"),
            sh("exit 0"),
            HostConfig {
                restart_policy: RestartPolicy::OnFailure {
                    max_retries: Some(1),
                },
                ..Default::default()
            },
        )
        .unwrap();
    machine.start().unwrap();
    assert_eq!(machine.wait_workload().unwrap().exit_code, 0);
    let old_pid = machine.inspect().unwrap().state.pid.unwrap();
    // SAFETY: this PID identifies only the test-owned VMM process group.
    unsafe {
        libc::kill(-(old_pid as i32), libc::SIGKILL);
    }
    wait_for("replacement after idle machine failure", || {
        let state = machine.inspect().unwrap().state;
        state.status == Status::Running && state.pid != Some(old_pid) && state.restart_count == 1
    });
    assert_eq!(machine.wait_workload().unwrap().exit_code, 0);
    machine.stop(Duration::from_secs(5)).unwrap();
}
