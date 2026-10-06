//! Seam A for the population boot through the helper: `VmspawnPopulator`
//! boots the clankerd-vmspawn helper against the blank disk, streams it the
//! merged tar and lets guestd unpack it. On Linux the helper's `--dev-local`
//! runs guestd as a local process where macOS would call libkrun, and a
//! directory stands in for the block device (mounting needs root; that part is
//! covered by the root-gated test and by seam B).

mod common;

use std::io::Cursor;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::registry::{Registry, layer};
use common::*;
use libclankerd::vmm::LocalProcessVmm;
use libclankerd::{DiskPopulator, Engine, ErrorKind, HostConfig, MachineConfig, VmspawnPopulator};

fn tar_of(files: &[(&str, &[u8], u32)]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for (path, data, mode) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(data.len() as u64);
        h.set_mode(*mode);
        h.set_uid(0);
        h.set_gid(0);
        h.set_mtime(1_700_000_000);
        b.append_data(&mut h, path, *data).unwrap();
    }
    b.into_inner().unwrap()
}

/// The directory the populator ships into the guest: guestd and mke2fs.
fn guest_dir() -> tempfile::TempDir {
    let boot = boot_dir_with_mke2fs();
    std::os::unix::fs::symlink(guestd_path(), boot.path().join("clankerd-guestd")).unwrap();
    boot
}

fn dev_populator(guest_dir: &Path) -> VmspawnPopulator {
    VmspawnPopulator::new(vmspawn_path(), guest_dir).dev_local()
}

#[test]
fn the_tar_is_unpacked_by_guestd_behind_the_helper() {
    let env = Env::new();
    let guest = guest_dir();
    let target = env.root().join("disk");
    std::fs::create_dir(&target).unwrap();

    let tar = tar_of(&[
        ("etc/hostname", b"box\n", 0o644),
        ("bin/tool", b"#!/bin/sh\n", 0o755),
    ]);
    dev_populator(guest.path())
        .populate(&target, 1 << 30, &mut Cursor::new(tar))
        .unwrap();

    assert_eq!(
        std::fs::read(target.join("etc/hostname")).unwrap(),
        b"box\n"
    );
    let mode = std::fs::metadata(target.join("bin/tool"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
}

#[test]
fn a_missing_mke2fs_is_reported_before_anything_boots() {
    let env = Env::new();
    let guest = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(guestd_path(), guest.path().join("clankerd-guestd")).unwrap();
    let target = env.root().join("disk");
    std::fs::create_dir(&target).unwrap();
    let err = dev_populator(guest.path())
        .populate(&target, 1 << 30, &mut Cursor::new(Vec::new()))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(err.message().contains("mke2fs"), "{err}");
}

#[test]
fn a_helper_that_dies_explains_why() {
    let env = Env::new();
    let guest = guest_dir();
    let fake = env.root().join("fake-vmspawn");
    std::fs::write(
        &fake,
        "#!/bin/sh\necho 'clankerd-vmspawn: the hypervisor entitlement is missing' >&2\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let target = env.root().join("disk");
    std::fs::create_dir(&target).unwrap();
    let err = VmspawnPopulator::new(fake, guest.path())
        .populate(&target, 1 << 30, &mut Cursor::new(Vec::new()))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(err.message().contains("entitlement is missing"), "{err}");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn the_real_helper_off_macos_explains_it_cannot_boot() {
    let env = Env::new();
    let guest = guest_dir();
    let target = env.root().join("disk");
    std::fs::create_dir(&target).unwrap();
    let err = VmspawnPopulator::new(vmspawn_path(), guest.path())
        .populate(&target, 1 << 30, &mut Cursor::new(Vec::new()))
        .unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(err.message().contains("only supported on macOS"), "{err}");
}

/// Root: an engine whose populator is the helper builds a real, clean ext4
/// root disk for a machine (loop-mounted by guestd standing in for the VM).
#[test]
fn engine_creates_a_machine_with_a_clean_populated_disk() {
    if !is_root() {
        eprintln!("SKIPPED engine_creates_a_machine_with_a_clean_populated_disk: needs root");
        return;
    }
    let env = Env::new();
    let registry = Registry::start();
    registry.push(
        "test/app",
        "v1",
        &[layer(&[("hello", b"hi"), ("etc/os-release", b"ID=test\n")])],
    );
    let guest = guest_dir();
    let mut cfg = env.config(Arc::new(LocalProcessVmm::new(guestd_path())));
    cfg.cache_dir = Some(env.root().join("cache"));
    cfg.populator = Some(Arc::new(dev_populator(guest.path())));
    cfg.insecure_registries = vec![registry.addr.clone()];
    let engine = Engine::new(cfg).unwrap();
    let m = engine
        .create(
            None,
            MachineConfig {
                image: format!("{}/test/app:v1", registry.addr),
                cmd: vec!["true".into()],
                ..Default::default()
            },
            HostConfig::default(),
        )
        .unwrap();
    let disk: PathBuf = env
        .root()
        .join("state/machines")
        .join(m.id())
        .join(libclankerd::ROOT_DISK);
    e2fsck_clean(&disk);
    assert!(debugfs(&disk, "stat /etc/os-release").contains("Type: regular"));
}
