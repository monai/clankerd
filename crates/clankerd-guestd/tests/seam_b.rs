//! Seam B: clankerd-guestd's varlink interface, real binary, nothing faked.
//!
//! Tests that need privileges (chown to foreign owners, mknod, trusted
//! xattrs, mounting) call `require_root!()` and print `SKIPPED ...` otherwise.
//! Everything else runs unprivileged.

mod common;

use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::Path;

use clankerd_proto::rootdisk::{METHOD_UNPACK_TAR, UnpackSummary, UnpackTar};
use common::tarbuild::{Meta, Tb};
use common::{Guestd, boot_dir_with_mke2fs};

/// Unpacks `tar` into a fresh directory through guestd and returns the directory.
fn unpack(tar: &[u8]) -> (tempfile::TempDir, clankerd_proto::varlink::Reply) {
    let boot = boot_dir_with_mke2fs();
    let guestd = Guestd::spawn(boot.path());
    let target = tempfile::Builder::new().prefix("root").tempdir().unwrap();
    let reply = guestd.call_with_stream(
        METHOD_UNPACK_TAR,
        &UnpackTar {
            target: target.path().to_str().unwrap().into(),
        },
        &mut &tar[..],
    );
    (target, reply)
}

fn unpack_ok(tar: &[u8]) -> tempfile::TempDir {
    let (dir, reply) = unpack(tar);
    assert_eq!(reply.error, None, "{:?}", reply.parameters);
    dir
}

fn lstat(root: &Path, rel: &str) -> fs::Metadata {
    fs::symlink_metadata(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn unpacks_files_dirs_and_symlinks_with_modes_sizes_and_mtimes() {
    let tar = Tb::new()
        .dir("etc/", Meta::default().mode(0o755).mtime(1_600_000_000))
        .file(
            "etc/passwd",
            b"root:x:0:0\n",
            Meta::default().mode(0o640).mtime(1_650_000_000),
        )
        .file("bin/tool", b"#!/bin/sh\n", Meta::default().mode(0o4755))
        .symlink("etc/link", "passwd", Meta::default().mode(0o777))
        .finish();

    let (dir, reply) = unpack(&tar);
    assert_eq!(reply.error, None);
    let summary: UnpackSummary = serde_json::from_value(reply.parameters).unwrap();
    assert_eq!(
        summary,
        UnpackSummary {
            entries: 4,
            bytes: 21
        }
    );

    let root = dir.path();
    let passwd = lstat(root, "etc/passwd");
    assert_eq!(passwd.permissions().mode() & 0o7777, 0o640);
    assert_eq!(passwd.len(), 11);
    assert_eq!(passwd.mtime(), 1_650_000_000);
    assert_eq!(fs::read(root.join("etc/passwd")).unwrap(), b"root:x:0:0\n");
    // Directory mtime survives its children being created after it.
    assert_eq!(lstat(root, "etc").mtime(), 1_600_000_000);
    assert_eq!(lstat(root, "etc").permissions().mode() & 0o7777, 0o755);
    // Missing parent dir of bin/tool is created implicitly.
    assert!(lstat(root, "bin").is_dir());
    assert_eq!(
        lstat(root, "bin/tool").permissions().mode() & 0o7777,
        0o4755
    );
    let link = lstat(root, "etc/link");
    assert!(link.file_type().is_symlink());
    assert_eq!(
        fs::read_link(root.join("etc/link")).unwrap().to_str(),
        Some("passwd")
    );
    assert_eq!(link.mtime(), 1_700_000_000);
}

#[test]
fn hardlinks_share_an_inode_and_fifos_are_created() {
    let tar = Tb::new()
        .file("a", b"data", Meta::default().mode(0o600))
        .hardlink("b", "a", Meta::default().mode(0o600))
        .fifo("pipe", Meta::default().mode(0o660))
        .finish();
    let dir = unpack_ok(&tar);
    let (a, b) = (lstat(dir.path(), "a"), lstat(dir.path(), "b"));
    assert_eq!(a.ino(), b.ino());
    assert_eq!(a.nlink(), 2);
    let pipe = lstat(dir.path(), "pipe");
    assert!(pipe.file_type().is_fifo());
    assert_eq!(pipe.permissions().mode() & 0o7777, 0o660);
}

#[test]
fn long_and_non_ascii_paths_and_link_targets_survive() {
    let long_dir = "d".repeat(90);
    let long_name = format!("{long_dir}/{}/{}", "e".repeat(90), "f".repeat(120));
    let target = "t".repeat(300);
    let tar = Tb::new()
        .file(&long_name, b"long", Meta::default())
        .file("héllo/世界/файл.txt", b"utf8", Meta::default())
        .symlink("longlink", &target, Meta::default())
        .finish();
    let dir = unpack_ok(&tar);
    assert_eq!(fs::read(dir.path().join(&long_name)).unwrap(), b"long");
    assert_eq!(
        fs::read(dir.path().join("héllo/世界/файл.txt")).unwrap(),
        b"utf8"
    );
    assert_eq!(
        fs::read_link(dir.path().join("longlink")).unwrap().to_str(),
        Some(target.as_str())
    );
}

#[test]
fn nanosecond_and_pre_epoch_mtimes_from_pax_are_kept() {
    let tar = Tb::new()
        .file(
            "ns",
            b"",
            Meta::default().pax("mtime", b"1700000000.123456789"),
        )
        .file("old", b"", Meta::default().pax("mtime", b"-86400.5"))
        .file(
            "future",
            b"",
            Meta::default().pax("mtime", b"4102444800.25"),
        )
        .finish();
    let dir = unpack_ok(&tar);
    let ns = lstat(dir.path(), "ns");
    assert_eq!((ns.mtime(), ns.mtime_nsec()), (1_700_000_000, 123_456_789));
    let old = lstat(dir.path(), "old");
    // -86400.5 s is 86401 s before the epoch plus half a second.
    assert_eq!((old.mtime(), old.mtime_nsec()), (-86_401, 500_000_000));
    let future = lstat(dir.path(), "future");
    assert_eq!(
        (future.mtime(), future.mtime_nsec()),
        (4_102_444_800, 250_000_000)
    );
}

#[test]
fn user_xattrs_including_binary_values_are_applied() {
    let tar = Tb::new()
        .file(
            "x",
            b"",
            Meta::default()
                .pax("SCHILY.xattr.user.text", b"hello")
                .pax("SCHILY.xattr.user.bin", &[0, 1, 2, 255]),
        )
        .finish();
    let dir = unpack_ok(&tar);
    match xattr_get(&dir.path().join("x"), "user.text") {
        Some(v) => {
            assert_eq!(v, b"hello");
            assert_eq!(
                xattr_get(&dir.path().join("x"), "user.bin").unwrap(),
                [0, 1, 2, 255]
            );
        }
        None => common::skip("user_xattrs", "filesystem under TMPDIR has no user xattrs"),
    }
}

#[test]
fn entries_escaping_the_target_are_rejected() {
    // A symlink pointing outside, then a file written "through" it.
    let outside = tempfile::tempdir().unwrap();
    let tar = Tb::new()
        .symlink("escape", outside.path().to_str().unwrap(), Meta::default())
        .file("escape/pwned", b"x", Meta::default())
        .finish();
    let (_dir, reply) = unpack(&tar);
    assert!(reply.error.is_some(), "expected an error reply");
    assert!(!outside.path().join("pwned").exists());

    let tar = Tb::new()
        .file_raw_name("../up", b"x", Meta::default())
        .finish();
    let (_dir, reply) = unpack(&tar);
    assert!(reply.error.is_some());
}

#[test]
fn unpack_into_a_missing_target_is_an_invalid_parameter() {
    let boot = boot_dir_with_mke2fs();
    let guestd = Guestd::spawn(boot.path());
    let reply = guestd.call_with_stream(
        METHOD_UNPACK_TAR,
        &UnpackTar {
            target: "/nonexistent-dir-for-clankerd".into(),
        },
        &mut std::io::empty(),
    );
    assert_eq!(
        reply.error.as_deref(),
        Some(clankerd_proto::rootdisk::ERROR_INVALID_PARAMETER)
    );
}

fn xattr_get(path: &Path, name: &str) -> Option<Vec<u8>> {
    use std::os::unix::ffi::OsStrExt;
    let p = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    let n = std::ffi::CString::new(name).unwrap();
    let mut buf = vec![0u8; 65536];
    // SAFETY: valid NUL-terminated strings and a writable buffer of the given length.
    let r = unsafe { libc::lgetxattr(p.as_ptr(), n.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if r < 0 {
        return None;
    }
    buf.truncate(r as usize);
    Some(buf)
}
