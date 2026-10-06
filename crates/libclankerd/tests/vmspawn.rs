//! Seam A over the real production path: Engine -> VmspawnVmm -> the
//! clankerd-vmspawn helper process -> guestd from a boot directory.
//!
//! Only the hypervisor is replaced: on Linux `clankerd-vmspawn --dev-local`
//! runs the boot directory's guestd as a local process where macOS would call
//! libkrun. Failure cases use scripts standing in for a vmspawn that dies.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use common::*;
use libclankerd::vmm::VmspawnVmm;
use libclankerd::{ErrorKind, Status};

fn built(package: &str) -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["build", "-q", "-p", package]);
    if profile_dir.file_name().is_some_and(|n| n == "release") {
        cmd.arg("--release");
    }
    assert!(cmd.status().unwrap().success(), "building {package} failed");
    profile_dir.join(package)
}

fn vmspawn_path() -> PathBuf {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| built("clankerd-vmspawn")).clone()
}

fn dev_vmm() -> Arc<VmspawnVmm> {
    Arc::new(VmspawnVmm::new(vmspawn_path(), guestd_path()).dev_local())
}

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-vmspawn");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn run_returns_the_exit_code_through_the_helper() {
    let env = Env::new();
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    let m = create(&engine, "viahelper", "echo booted; exit 5");
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 5);
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
}

#[test]
fn logs_show_what_the_guest_printed() {
    let env = Env::new();
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    let m = create(&engine, "chatty", "echo from-the-guest");
    m.start().unwrap();
    m.wait().unwrap();
    let logs = String::from_utf8(m.logs().unwrap()).unwrap();
    assert!(logs.contains("from-the-guest"), "{logs}");
}

#[test]
fn the_helper_outlives_the_library_process() {
    let env = Env::new();
    let marker = env.root().join("go");
    {
        let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
        let m = create(
            &engine,
            "survivor",
            &format!(
                "while [ ! -e {} ]; do sleep 0.05; done; exit 9",
                marker.display()
            ),
        );
        m.start().unwrap();
    }
    // A new Engine stands in for a restarted library process.
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    let m = engine.get("survivor").unwrap();
    assert_eq!(m.inspect().unwrap().state.status, Status::Running);
    std::fs::write(&marker, "").unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 9);
}

#[test]
fn missing_libkrun_fails_fast_with_install_instructions() {
    let env = Env::new();
    let fake = script(
        env.root(),
        "echo 'dyld[42]: Library not loaded: /opt/homebrew/lib/libkrun.1.dylib' >&2; exit 1",
    );
    let vmm = Arc::new(VmspawnVmm::new(fake, guestd_path()));
    let mut cfg = env.config(vmm);
    cfg.start_timeout = Duration::from_secs(20);
    let engine = libclankerd::Engine::new(cfg).unwrap();
    let m = create(&engine, "nolib", "exit 0");

    let began = Instant::now();
    let err = m.start().unwrap_err();
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "should not wait for the start timeout"
    );
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(err.message().contains("libkrun is not installed"), "{err}");
    assert!(err.message().contains("brew"), "{err}");
    assert_eq!(m.inspect().unwrap().state.status, Status::Created);
}

#[test]
fn helper_failure_text_reaches_the_caller() {
    let env = Env::new();
    let fake = script(
        env.root(),
        "echo 'clankerd-vmspawn: the hypervisor entitlement is missing: sign it' >&2; exit 1",
    );
    let engine =
        libclankerd::Engine::new(env.config(Arc::new(VmspawnVmm::new(fake, guestd_path()))))
            .unwrap();
    let m = create(&engine, "noent", "exit 0");
    let err = m.start().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(
        err.message().contains("hypervisor entitlement is missing"),
        "{err}"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn the_real_helper_off_macos_explains_it_cannot_boot() {
    let env = Env::new();
    let vmm = Arc::new(VmspawnVmm::new(vmspawn_path(), guestd_path()));
    let engine = libclankerd::Engine::new(env.config(vmm)).unwrap();
    let m = create(&engine, "linux", "exit 0");
    let err = m.start().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(err.message().contains("only supported on macOS"), "{err}");
}
