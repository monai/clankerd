//! Seam B on real images: pull from the public registry, merge the layers with
//! ocirender, unpack the merged tar through clankerd-guestd's varlink interface,
//! and compare the result with the tar itself.
//!
//! Needs network, so it is opt-in: `CLANKERD_TEST_NETWORK=1`. As root the
//! comparison covers owners, device nodes and file capabilities too; without
//! root guestd runs with `--lenient-ownership` and the report says what was
//! not checked. `CLANKERD_TEST_CACHE_DIR` keeps pulled blobs between runs.

mod common;

use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use clankerd_proto::rootdisk::{METHOD_UNPACK_TAR, UnpackSummary, UnpackTar};
use clankerd_proto::varlink::{self, Call, Reply};
use common::tarcompare::compare;
use common::{Env, TarCapture, guestd_path};
use libclankerd::vmm::LocalProcessVmm;
use libclankerd::{Engine, HostConfig, MachineConfig, ROOT_DISK};

fn is_root() -> bool {
    // SAFETY: no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Unpacks `tar` into `target` via a real guestd; returns its summary.
fn guestd_unpack(tar: &Path, target: &Path, lenient: bool) -> UnpackSummary {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("g.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let fd = listener.as_raw_fd();
    let mut cmd = Command::new(guestd_path());
    cmd.args(["--populate", "--boot-dir", "/nonexistent"])
        .env("LISTEN_FDS", "1")
        .stdin(Stdio::null());
    if lenient {
        cmd.arg("--lenient-ownership");
    }
    // SAFETY: only async-signal-safe calls between fork and exec.
    unsafe {
        cmd.pre_exec(move || {
            if fd == 3 {
                let flags = libc::fcntl(3, libc::F_GETFD);
                libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            } else if libc::dup2(fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();

    let mut conn = UnixStream::connect(&socket).unwrap();
    varlink::write(
        &mut conn,
        &Call {
            method: METHOD_UNPACK_TAR.into(),
            parameters: serde_json::to_value(UnpackTar {
                target: target.to_string_lossy().into_owned(),
            })
            .unwrap(),
            more: false,
        },
    )
    .unwrap();
    std::io::copy(&mut std::fs::File::open(tar).unwrap(), &mut conn).unwrap();
    conn.shutdown(std::net::Shutdown::Write).unwrap();
    let reply: Reply = varlink::read(&mut std::io::BufReader::new(conn))
        .unwrap()
        .expect("guestd replied");
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(reply.error, None, "{:?}", reply.parameters);
    serde_json::from_value(reply.parameters).unwrap()
}

fn cache_dir(env: &Env) -> PathBuf {
    std::env::var_os("CLANKERD_TEST_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| env.root().join("cache"))
}

fn check_real_image(reference: &str) {
    if std::env::var_os("CLANKERD_TEST_NETWORK").is_none() {
        eprintln!(
            "SKIPPED seam_b_images {reference}: set CLANKERD_TEST_NETWORK=1 (needs registry access)"
        );
        return;
    }
    let env = Env::new();
    let populator = Arc::new(TarCapture::default());
    let mut cfg = env.config(Arc::new(LocalProcessVmm::new(guestd_path())));
    cfg.cache_dir = Some(cache_dir(&env));
    cfg.populator = Some(populator.clone());
    let engine = Engine::new(cfg).unwrap();

    let machine = engine
        .create(
            Some("real"),
            MachineConfig {
                image: reference.into(),
                cmd: vec!["true".into()],
                ..Default::default()
            },
            HostConfig::default(),
        )
        .unwrap();
    assert_eq!(populator.runs.load(Ordering::SeqCst), 1);
    let info = machine.inspect().unwrap();
    assert!(info.image_id.starts_with("sha256:"), "{}", info.image_id);
    let tar = env
        .root()
        .join("state/machines")
        .join(machine.id())
        .join(ROOT_DISK);

    let unpacked = tempfile::Builder::new().prefix("img").tempdir().unwrap();
    let privileged = is_root();
    let summary = guestd_unpack(&tar, unpacked.path(), !privileged);
    let report = compare(&tar, unpacked.path(), privileged);

    eprintln!(
        "{reference} ({}): {} tar entries compared, {} files hashed, {} hardlinks, {} devices, \
         {} xattrs ({} capabilities); guestd unpacked {} entries / {} bytes; not checked: {:?}",
        info.image_id,
        report.entries,
        report.regular_files,
        report.hardlinks,
        report.devices,
        report.xattrs,
        report.capabilities,
        summary.entries,
        summary.bytes,
        report.skipped
    );
    assert!(report.entries > 100, "suspiciously small image");
    let shown: Vec<_> = report.mismatches.iter().take(20).collect();
    assert!(
        report.mismatches.is_empty(),
        "{} mismatches, first: {shown:#?}",
        report.mismatches.len()
    );
}

#[test]
fn debian_trixie_slim_unpacks_exactly() {
    check_real_image("debian:trixie-slim");
}

#[test]
fn clankers_slim_unpacks_exactly() {
    check_real_image("ghcr.io/monai/clankers:slim");
}
