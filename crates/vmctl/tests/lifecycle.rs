//! CLI: detached runs, `create`/`start`, `stop` (graceful, kill after the
//! timeout) and `kill`, and the `run` flags that shape the workload
//! (`--entrypoint`, `-e`, `-u`, `-w`). The VMM is the local-process stand-in.

#[path = "../../libclankerd/tests/common/registry.rs"]
mod registry;

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use common::{guestd, text, vmctl, wait_for};

struct Cli {
    dir: tempfile::TempDir,
    guestd: PathBuf,
}

impl Cli {
    fn new() -> Self {
        Cli {
            dir: tempfile::Builder::new().prefix("vm").tempdir().unwrap(),
            guestd: guestd(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        vmctl(self.root(), &self.guestd, args)
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn status_of(&self, name: &str) -> String {
        let out = self.run(&["inspect", name]);
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        format!(
            "{} {}",
            v[0]["state"]["status"].as_str().unwrap(),
            v[0]["state"]["exit_code"]
        )
    }

    /// Waits for the workload to say it is ready.
    fn wait_for(&self, file: &str) {
        let path = self.root().join(file);
        wait_for(file, || path.exists());
    }
}

#[test]
fn run_detached_returns_at_once_and_stop_ends_the_machine_gracefully() {
    let cli = Cli::new();
    let ready = cli.root().join("ready");
    let script = format!("touch {}; sleep 60", ready.display());
    let out = cli.run(&["run", "-d", "--name", "dev", "img", "sh", "-c", &script]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(text(&out.stdout).trim().len(), 64, "prints the machine id");
    cli.wait_for("ready");
    assert!(text(&cli.run(&["ps"]).stdout).contains("Up"));

    let stop = cli.run(&["stop", "dev"]);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    assert_eq!(text(&stop.stdout).trim(), "dev");
    assert_eq!(cli.status_of("dev"), "exited 143");
}

#[test]
fn stop_kills_after_the_timeout_when_the_workload_ignores_sigterm() {
    let cli = Cli::new();
    let ready = cli.root().join("ready");
    let script = format!(
        "trap '' TERM; touch {}; while true; do sleep 0.05; done",
        ready.display()
    );
    assert!(
        cli.run(&[
            "run", "-d", "--name", "stubborn", "img", "sh", "-c", &script
        ])
        .status
        .success()
    );
    cli.wait_for("ready");

    let began = Instant::now();
    let stop = cli.run(&["stop", "-t", "1", "stubborn"]);
    assert!(stop.status.success(), "{}", text(&stop.stderr));
    assert!(began.elapsed() >= Duration::from_secs(1));
    assert_eq!(cli.status_of("stubborn"), "exited 137");
}

#[test]
fn kill_defaults_to_sigkill_and_accepts_a_signal() {
    let cli = Cli::new();
    let script = |name: &str| {
        format!(
            "trap 'exit 7' USR1; touch {}; while true; do sleep 0.05; done",
            cli.root().join(name).display()
        )
    };
    for name in ["a", "b"] {
        assert!(
            cli.run(&[
                "run",
                "-d",
                "--name",
                name,
                "img",
                "sh",
                "-c",
                &script(name)
            ])
            .status
            .success()
        );
        cli.wait_for(name);
    }
    assert!(cli.run(&["kill", "a"]).status.success());
    assert_eq!(cli.status_of("a"), "exited 137");

    assert!(cli.run(&["kill", "-s", "USR1", "b"]).status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while cli.status_of("b") != "exited 7" {
        assert!(Instant::now() < deadline, "b is {}", cli.status_of("b"));
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn killing_or_stopping_unknown_machines_fails() {
    let cli = Cli::new();
    assert_eq!(cli.run(&["kill", "ghost"]).status.code(), Some(1));
    assert_eq!(cli.run(&["stop", "ghost"]).status.code(), Some(1));
    assert!(text(&cli.run(&["stop", "ghost"]).stderr).contains("no such machine"));
}

#[test]
fn create_then_start_runs_the_machine_and_it_can_start_again() {
    let cli = Cli::new();
    let created = cli.run(&["create", "--name", "later", "img", "sh", "-c", "exit 4"]);
    assert!(created.status.success());
    assert_eq!(cli.status_of("later"), "created 0");

    assert!(cli.run(&["start", "later"]).status.success());
    let deadline = Instant::now() + Duration::from_secs(10);
    while cli.status_of("later") != "exited 4" {
        assert!(
            Instant::now() < deadline,
            "later is {}",
            cli.status_of("later")
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(cli.run(&["start", "later"]).status.success());
}

#[test]
fn with_the_libkrun_backend_run_pulls_the_image_and_needs_the_guest_boot_directory() {
    let cli = Cli::new();
    let reg = registry::Registry::start();
    reg.push("test/app", "v1", &[registry::layer(&[("hello", b"hi")])]);
    // No dev stand-in: vmctl is configured for the real helper. The directory
    // holding guestd lacks mke2fs, so the base disk cannot be built, which is
    // exactly what a half-built `make rust` output looks like.
    let boot = cli.root().join("linux-arm64");
    std::fs::create_dir(&boot).unwrap();
    std::fs::copy(&cli.guestd, boot.join("clankerd-guestd")).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .args(["run", &format!("{}/test/app:v1", reg.addr), "true"])
        .env("CLANKERD_STATE_DIR", cli.root().join("state"))
        .env("CLANKERD_RUNTIME_DIR", cli.root().join("run"))
        .env("CLANKERD_CACHE_DIR", cli.root().join("cache"))
        .env("CLANKERD_INSECURE_REGISTRIES", &reg.addr)
        .env("CLANKERD_VMSPAWN", "/no/such/clankerd-vmspawn")
        .env("CLANKERD_GUESTD", boot.join("clankerd-guestd"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(125));
    let err = text(&out.stderr);
    assert!(err.contains("mke2fs") && err.contains("make rust"), "{err}");
    // The image was pulled on the way.
    let cached = std::fs::read_dir(cli.root().join("cache/images"))
        .unwrap()
        .count();
    assert_eq!(cached, 1, "the image should be in the cache");
}

#[test]
fn entrypoint_env_user_and_workdir_flags_shape_the_workload() {
    let cli = Cli::new();
    let out = cli.root().join("out");
    // SAFETY: no preconditions.
    let (uid, gid) = unsafe { (libc::getuid(), libc::getgid()) };
    let user = format!("{uid}:{gid}");
    let script = format!("echo \"$GREETING $PWD $(id -u)\" > {}", out.display());
    let run = cli.run(&[
        "run",
        "-e",
        "GREETING=hello",
        "-u",
        &user,
        "-w",
        "/tmp",
        "--entrypoint",
        "/bin/sh",
        "img",
        "-c",
        &script,
    ]);
    assert_eq!(run.status.code(), Some(0), "{}", text(&run.stderr));
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        format!("hello /tmp {uid}\n")
    );
}

#[test]
fn run_restart_on_failure_reruns_the_workload_and_records_the_restarts() {
    let cli = Cli::new();
    let log = cli.root().join("log");
    let script = format!("echo run >> {}; exit 5", log.display());
    let out = cli.run(&[
        "run",
        "--restart",
        "on-failure:2",
        "--name",
        "flaky",
        "img",
        "sh",
        "-c",
        &script,
    ]);
    assert_eq!(out.status.code(), Some(5), "{}", text(&out.stderr));
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 3);
    let inspect = cli.run(&["inspect", "flaky"]);
    let v: serde_json::Value = serde_json::from_slice(&inspect.stdout).unwrap();
    assert_eq!(v[0]["state"]["restart_count"], 2);
    assert_eq!(
        v[0]["host_config"]["restart_policy"]["on-failure"]["max_retries"],
        2
    );
}

#[test]
fn invalid_restart_and_pull_policies_are_usage_errors() {
    let cli = Cli::new();
    let out = cli.run(&["create", "--restart", "sometimes", "img", "true"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("invalid restart policy"),
        "{}",
        text(&out.stderr)
    );
    let out = cli.run(&["create", "--pull", "sometimes", "img", "true"]);
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("invalid pull policy"),
        "{}",
        text(&out.stderr)
    );
}
