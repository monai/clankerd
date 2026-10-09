//! Seam B: clankerd-guestd's varlink interface, real binary, nothing faked.
//!
//! Tests that need privileges (chown to foreign owners, mknod, trusted
//! xattrs, mounting) call `require_root!()` and print `SKIPPED ...` otherwise.
//! Everything else runs unprivileged.

mod common;

use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

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

// ---- formatting ----

use clankerd_proto::rootdisk::{
    FormatExt4, METHOD_FORMAT_EXT4, METHOD_POPULATE_DISK, PopulateDisk,
};
use std::process::Command;

fn tool(name: &str) -> String {
    std::env::var_os("CLANKERD_TEST_BOOT_DIR")
        .map(|dir| PathBuf::from(dir).join(name).to_string_lossy().into_owned())
        .into_iter()
        .chain(
            ["/usr/sbin", "/sbin", "/usr/bin"]
                .into_iter()
                .map(|dir| format!("{dir}/{name}")),
        )
        .find(|path| Path::new(path).exists())
        .unwrap_or_else(|| panic!("{name} (e2fsprogs) must be installed to run seam B tests"))
}

fn debugfs(image: &Path, request: &str) -> String {
    let out = Command::new(tool("debugfs"))
        .args(["-R", request])
        .arg(image)
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn e2fsck_clean(image: &Path) {
    let out = Command::new(tool("e2fsck"))
        .arg("-fn")
        .arg(image)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "e2fsck -fn failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn formatted_image_is_clean_ext4_of_the_requested_size() {
    let boot = boot_dir_with_mke2fs();
    let guestd = Guestd::spawn(boot.path());
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("root.ext4");
    fs::File::create(&image).unwrap();

    let reply = guestd.call(
        METHOD_FORMAT_EXT4,
        &FormatExt4 {
            device: image.to_str().unwrap().into(),
            size: Some(64 << 20),
            label: Some("clankerd".into()),
        },
    );
    assert_eq!(reply.error, None, "{:?}", reply.parameters);

    assert_eq!(fs::metadata(&image).unwrap().len(), 64 << 20);
    // Sparse: formatting must not allocate the whole file.
    assert!(fs::metadata(&image).unwrap().blocks() * 512 < 32 << 20);
    e2fsck_clean(&image);
    let stats = debugfs(&image, "stats");
    assert!(
        stats.contains("Filesystem volume name:   clankerd"),
        "{stats}"
    );
    assert!(stats.contains("extent"), "{stats}");
    assert!(stats.contains("Block size:               4096"), "{stats}");
}

#[test]
fn formatting_without_a_shipped_mke2fs_fails_with_a_clear_message() {
    let empty_boot = tempfile::tempdir().unwrap();
    let guestd = Guestd::spawn(empty_boot.path());
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("root.ext4");
    fs::File::create(&image).unwrap();
    let reply = guestd.call(
        METHOD_FORMAT_EXT4,
        &FormatExt4 {
            device: image.to_str().unwrap().into(),
            size: Some(8 << 20),
            label: None,
        },
    );
    assert_eq!(
        reply.error.as_deref(),
        Some(clankerd_proto::rootdisk::ERROR_FAILED)
    );
    assert!(
        reply.parameters["message"]
            .as_str()
            .unwrap()
            .contains("mke2fs")
    );
}

#[test]
fn formatting_a_missing_device_is_an_invalid_parameter() {
    let boot = boot_dir_with_mke2fs();
    let guestd = Guestd::spawn(boot.path());
    let reply = guestd.call(
        METHOD_FORMAT_EXT4,
        &FormatExt4 {
            device: "/nonexistent/disk".into(),
            size: None,
            label: None,
        },
    );
    assert_eq!(
        reply.error.as_deref(),
        Some(clankerd_proto::rootdisk::ERROR_INVALID_PARAMETER)
    );
}

// ---- root only ----

fn dev_major(dev: u64) -> u64 {
    ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0xfff)
}

fn dev_minor(dev: u64) -> u64 {
    ((dev >> 12) & 0xffff_ff00) | (dev & 0xff)
}

#[test]
fn root_owners_devices_setuid_and_file_capabilities_are_reproduced() {
    require_root!();
    // CAP_NET_RAW (bit 13) in vfs_cap_data revision 2 layout.
    let cap: Vec<u8> = [
        &0x0200_0001u32.to_le_bytes()[..],
        &0x0000_2000u32.to_le_bytes(),
        &0u32.to_le_bytes(),
        &0u32.to_le_bytes(),
        &0u32.to_le_bytes(),
    ]
    .concat();
    let tar = Tb::new()
        .file(
            "usr/bin/ping",
            b"elf",
            Meta::default()
                .mode(0o4755)
                .owner(0, 0)
                .pax("SCHILY.xattr.security.capability", &cap),
        )
        .file(
            "home/a/f",
            b"x",
            Meta::default().mode(0o600).owner(1001, 2002),
        )
        .file(
            "big",
            b"x",
            Meta::default()
                .pax("uid", b"4000000000")
                .pax("gid", b"4000000001"),
        )
        .device(
            "dev/null",
            false,
            1,
            3,
            Meta::default().mode(0o666).owner(0, 0),
        )
        .device(
            "dev/loop0",
            true,
            7,
            0,
            Meta::default().mode(0o660).owner(0, 6),
        )
        .dir("home/a", Meta::default().mode(0o700).owner(1001, 2002))
        .finish();
    let dir = unpack_ok(&tar);
    let root = dir.path();

    let ping = lstat(root, "usr/bin/ping");
    assert_eq!((ping.uid(), ping.gid()), (0, 0));
    assert_eq!(ping.permissions().mode() & 0o7777, 0o4755);
    assert_eq!(
        xattr_get(&root.join("usr/bin/ping"), "security.capability").unwrap(),
        cap
    );

    let f = lstat(root, "home/a/f");
    assert_eq!((f.uid(), f.gid()), (1001, 2002));
    let d = lstat(root, "home/a");
    assert_eq!((d.uid(), d.gid()), (1001, 2002));
    assert_eq!(d.permissions().mode() & 0o7777, 0o700);
    let big = lstat(root, "big");
    assert_eq!((big.uid(), big.gid()), (4_000_000_000, 4_000_000_001));

    let null = lstat(root, "dev/null");
    assert!(null.file_type().is_char_device());
    assert_eq!((dev_major(null.rdev()), dev_minor(null.rdev())), (1, 3));
    let loop0 = lstat(root, "dev/loop0");
    assert!(loop0.file_type().is_block_device());
    assert_eq!((dev_major(loop0.rdev()), dev_minor(loop0.rdev())), (7, 0));
    assert_eq!(loop0.gid(), 6);
}

#[test]
fn populate_disk_formats_mounts_unpacks_and_leaves_a_clean_image() {
    require_root!();
    let tar = Tb::new()
        .dir("etc/", Meta::default().mode(0o755).owner(0, 0))
        .file(
            "etc/shadow",
            b"secret",
            Meta::default().mode(0o640).owner(0, 42),
        )
        .symlink("etc/alias", "shadow", Meta::default().owner(0, 0))
        .device(
            "dev/null",
            false,
            1,
            3,
            Meta::default().mode(0o666).owner(0, 0),
        )
        .finish();
    let boot = boot_dir_with_mke2fs();
    let guestd = Guestd::spawn(boot.path());
    let dir = tempfile::tempdir().unwrap();
    let image = dir.path().join("root.ext4");
    fs::File::create(&image).unwrap();

    let reply = guestd.call_with_stream(
        METHOD_POPULATE_DISK,
        &PopulateDisk {
            device: image.to_str().unwrap().into(),
            size: Some(64 << 20),
        },
        &mut &tar[..],
    );
    assert_eq!(reply.error, None, "{:?}", reply.parameters);
    e2fsck_clean(&image);

    let stat = debugfs(&image, "stat /etc/shadow");
    assert!(stat.contains("Mode:  0640"), "{stat}");
    assert!(stat.contains("User:     0   Group:    42"), "{stat}");
    assert!(stat.contains("Size: 6"), "{stat}");
    let link = debugfs(&image, "stat /etc/alias");
    assert!(link.contains("Type: symlink"), "{link}");
    assert!(link.contains("Fast link dest: \"shadow\""), "{link}");
    let null = debugfs(&image, "stat /dev/null");
    assert!(null.contains("Type: character special"), "{null}");
    assert!(null.contains("Device major/minor number: 01:03"), "{null}");
}
