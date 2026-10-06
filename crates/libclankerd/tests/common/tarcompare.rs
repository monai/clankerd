//! Compares an unpacked directory against the tar it came from. The tar's own
//! headers are the oracle: every path, type, mode, owner, size, symlink target,
//! hardlink, device node, xattr, mtime and file content is checked.
#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CString;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};
use tar::EntryType;

#[derive(Debug, Default)]
pub struct Report {
    pub entries: usize,
    pub regular_files: usize,
    pub hardlinks: usize,
    pub devices: usize,
    pub xattrs: usize,
    pub capabilities: usize,
    /// Human-readable list of every mismatch (empty when the tree is identical).
    pub mismatches: Vec<String>,
    /// Checks that were not performed, and why.
    pub skipped: Vec<String>,
}

/// `privileged`: the unpack ran as root, so owners, device nodes and
/// privileged xattrs must match too.
pub fn compare(tar_path: &Path, root: &Path, privileged: bool) -> Report {
    let mut r = Report::default();
    if !privileged {
        r.skipped
            .push("owners, device nodes and security.* xattrs (not root)".into());
    }
    let mut expected: BTreeSet<PathBuf> = BTreeSet::new();
    let mut ar = tar::Archive::new(std::fs::File::open(tar_path).unwrap());
    // Last entry for a path wins, like an unpack does.
    let mut last: BTreeMap<PathBuf, usize> = BTreeMap::new();
    let mut index = 0;
    for e in ar.entries().unwrap() {
        let e = e.unwrap();
        last.insert(rel(&e.path_bytes()), index);
        index += 1;
    }
    let mut ar = tar::Archive::new(std::fs::File::open(tar_path).unwrap());
    for (i, e) in ar.entries().unwrap().enumerate() {
        let mut e = e.unwrap();
        let ty = e.header().entry_type();
        if matches!(ty, EntryType::XGlobalHeader | EntryType::XHeader) {
            continue;
        }
        let path = rel(&e.path_bytes());
        for anc in path.ancestors() {
            expected.insert(anc.to_path_buf());
        }
        if last[&path] != i || path.as_os_str().is_empty() {
            continue; // superseded entry, or the root itself
        }
        r.entries += 1;
        if !privileged && matches!(ty, EntryType::Char | EntryType::Block) {
            continue; // a lenient unpack cannot mknod
        }
        let abs = root.join(&path);
        let meta = match std::fs::symlink_metadata(&abs) {
            Ok(m) => m,
            Err(err) => {
                r.mismatches
                    .push(format!("{}: missing ({err})", path.display()));
                continue;
            }
        };
        let mut bad = |what: String| r.mismatches.push(format!("{}: {what}", path.display()));
        let h = e.header().clone();
        let ft = meta.file_type();
        let type_ok = match ty {
            EntryType::Directory => ft.is_dir(),
            EntryType::Regular | EntryType::Continuous => ft.is_file(),
            EntryType::Symlink => ft.is_symlink(),
            EntryType::Fifo => ft.is_fifo(),
            EntryType::Char => ft.is_char_device(),
            EntryType::Block => ft.is_block_device(),
            EntryType::Link => true,
            _ => true,
        };
        if !type_ok {
            bad(format!("type {ty:?} but found {ft:?}"));
            continue;
        }

        let mut pax_mtime = None;
        let mut pax_uid = None;
        let mut pax_gid = None;
        let mut xattrs: Vec<(String, Vec<u8>)> = Vec::new();
        if let Some(exts) = e.pax_extensions().unwrap() {
            for x in exts {
                let x = x.unwrap();
                let (k, v) = (x.key().unwrap().to_owned(), x.value_bytes().to_vec());
                match k.as_str() {
                    "mtime" => pax_mtime = Some(String::from_utf8_lossy(&v).into_owned()),
                    "uid" => pax_uid = String::from_utf8_lossy(&v).parse::<u64>().ok(),
                    "gid" => pax_gid = String::from_utf8_lossy(&v).parse::<u64>().ok(),
                    _ => {
                        if let Some(name) = k.strip_prefix("SCHILY.xattr.") {
                            xattrs.push((name.to_owned(), v));
                        }
                    }
                }
            }
        }

        if ty == EntryType::Link {
            r.hardlinks += 1;
            let target = rel(&e.link_name_bytes().unwrap());
            match std::fs::symlink_metadata(root.join(&target)) {
                Ok(t) if t.ino() == meta.ino() && t.dev() == meta.dev() => {}
                Ok(_) => bad(format!(
                    "hardlink to {} does not share its inode",
                    target.display()
                )),
                Err(err) => bad(format!(
                    "hardlink target {} missing ({err})",
                    target.display()
                )),
            }
            continue;
        }

        if ty != EntryType::Symlink {
            let want = h.mode().unwrap() & 0o7777;
            let got = meta.mode() & 0o7777;
            if want != got {
                bad(format!("mode {want:o} but found {got:o}"));
            }
        }
        if privileged {
            let uid = pax_uid.unwrap_or_else(|| h.uid().unwrap());
            let gid = pax_gid.unwrap_or_else(|| h.gid().unwrap());
            if (uid, gid) != (meta.uid() as u64, meta.gid() as u64) {
                bad(format!(
                    "owner {uid}:{gid} but found {}:{}",
                    meta.uid(),
                    meta.gid()
                ));
            }
        }
        let want_mtime = match &pax_mtime {
            Some(s) => pax_time(s),
            None => (h.mtime().unwrap() as i64, 0),
        };
        if want_mtime != (meta.mtime(), meta.mtime_nsec()) {
            bad(format!(
                "mtime {want_mtime:?} but found {:?}",
                (meta.mtime(), meta.mtime_nsec())
            ));
        }
        match ty {
            EntryType::Regular | EntryType::Continuous => {
                r.regular_files += 1;
                if e.size() != meta.len() {
                    bad(format!("size {} but found {}", e.size(), meta.len()));
                } else if sha256_reader(&mut e) != sha256_file(&abs) {
                    bad("content differs".into());
                }
            }
            EntryType::Symlink => {
                let want = rel_raw(&e.link_name_bytes().unwrap());
                let got = std::fs::read_link(&abs).unwrap();
                if want.as_bytes() != got.as_os_str().as_bytes() {
                    bad(format!("symlink to {:?} but found {:?}", want, got));
                }
            }
            EntryType::Char | EntryType::Block if privileged => {
                r.devices += 1;
                let want = (
                    h.device_major().unwrap().unwrap() as u64,
                    h.device_minor().unwrap().unwrap() as u64,
                );
                let got = (dev_major(meta.rdev()), dev_minor(meta.rdev()));
                if want != got {
                    bad(format!("device {want:?} but found {got:?}"));
                }
            }
            _ => {}
        }
        for (name, value) in &xattrs {
            if !privileged && (name.starts_with("security.") || name.starts_with("trusted.")) {
                continue;
            }
            r.xattrs += 1;
            if name == "security.capability" {
                r.capabilities += 1;
            }
            match lgetxattr(&abs, name) {
                Some(got) if &got == value => {}
                Some(got) => bad(format!("xattr {name} is {got:?}, want {value:?}")),
                None => bad(format!("xattr {name} missing")),
            }
        }
    }

    // Nothing extra: every path found on disk must come from the tar.
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let p = entry.path();
            let relp = p.strip_prefix(root).unwrap().to_path_buf();
            if !expected.contains(&relp) {
                r.mismatches
                    .push(format!("{}: not in the tar", relp.display()));
            }
            if entry.file_type().unwrap().is_dir() {
                stack.push(p);
            }
        }
    }
    r
}

fn rel(raw: &[u8]) -> PathBuf {
    let mut out = PathBuf::new();
    for c in Path::new(std::ffi::OsStr::from_bytes(raw)).components() {
        if let Component::Normal(c) = c {
            out.push(c);
        }
    }
    out
}

fn rel_raw(raw: &[u8]) -> std::ffi::OsString {
    std::ffi::OsStr::from_bytes(raw).to_owned()
}

fn pax_time(s: &str) -> (i64, i64) {
    let (neg, s) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s),
    };
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let secs: i64 = int.parse().unwrap();
    let nanos: i64 = format!("{frac:0<9}")[..9].parse().unwrap();
    match (neg, nanos) {
        (false, n) => (secs, n),
        (true, 0) => (-secs, 0),
        (true, n) => (-secs - 1, 1_000_000_000 - n),
    }
}

fn dev_major(dev: u64) -> u64 {
    ((dev >> 32) & 0xffff_f000) | ((dev >> 8) & 0xfff)
}

fn dev_minor(dev: u64) -> u64 {
    ((dev >> 12) & 0xffff_ff00) | (dev & 0xff)
}

fn sha256_reader(r: &mut dyn Read) -> Vec<u8> {
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = r.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    h.finalize().to_vec()
}

fn sha256_file(p: &Path) -> Vec<u8> {
    match std::fs::File::open(p) {
        Ok(mut f) => sha256_reader(&mut f),
        Err(_) => Vec::new(),
    }
}

pub fn lgetxattr(path: &Path, name: &str) -> Option<Vec<u8>> {
    let p = CString::new(path.as_os_str().as_bytes()).unwrap();
    let n = CString::new(name).unwrap();
    let mut buf = vec![0u8; 65536];
    // SAFETY: valid NUL-terminated strings and a writable buffer of the given length.
    let r = unsafe { libc::lgetxattr(p.as_ptr(), n.as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
    if r < 0 {
        return None;
    }
    buf.truncate(r as usize);
    Some(buf)
}
