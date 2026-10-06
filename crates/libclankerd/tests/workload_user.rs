//! Seam A: `-u` applies to the workload (not only to exec). The tests run
//! unprivileged, so the only identity they can switch to is their own; what
//! they can show is that the user is resolved and applied, and that failing to
//! switch is an error rather than silently running as someone else.

mod common;

use common::*;
use libclankerd::{HostConfig, MachineConfig};

fn me() -> u32 {
    // SAFETY: no preconditions.
    unsafe { libc::getuid() }
}

fn run_as(env: &Env, user: &str, script: &str) -> (i32, String) {
    let engine = env.engine();
    let cfg = MachineConfig {
        user: user.into(),
        ..sh(script)
    };
    let m = engine.create(None, cfg, HostConfig::default()).unwrap();
    m.start().unwrap();
    let code = m.wait().unwrap().exit_code;
    (
        code,
        String::from_utf8_lossy(&m.logs().unwrap()).into_owned(),
    )
}

#[test]
fn the_workload_runs_as_the_requested_user_and_group() {
    let env = Env::new();
    let out = env.root().join("who");
    let (uid, gid) = (me(), unsafe { libc::getgid() });
    let (code, _) = run_as(
        &env,
        &format!("{uid}:{gid}"),
        &format!("echo \"$(id -u):$(id -g)\" > {}", out.display()),
    );
    assert_eq!(code, 0);
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        format!("{uid}:{gid}\n")
    );
}

#[test]
fn home_comes_from_the_user_unless_the_image_sets_it() {
    let env = Env::new();
    let out = env.root().join("home");
    let (code, _) = run_as(
        &env,
        &me().to_string(),
        &format!("echo \"$HOME\" > {}", out.display()),
    );
    assert_eq!(code, 0);
    let home = std::fs::read_to_string(&out).unwrap();
    assert!(home.trim().starts_with('/'), "HOME={home:?}");
}

#[test]
fn a_user_that_cannot_be_switched_to_fails_the_machine_with_exit_126() {
    let env = Env::new();
    // The tests are not root, so becoming root must fail.
    let (code, logs) = run_as(&env, "0", "echo should not run");
    assert_eq!(code, 126, "{logs}");
    assert!(!logs.contains("should not run"));
}

#[test]
fn an_unknown_user_name_fails_the_machine_with_exit_126_and_says_so() {
    let env = Env::new();
    let (code, logs) = run_as(&env, "no-such-user-here", "true");
    assert_eq!(code, 126);
    assert!(logs.contains("no-such-user-here"), "{logs}");
}
