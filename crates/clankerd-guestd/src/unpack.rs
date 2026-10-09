//! Tar unpacking with real Linux semantics: owners, modes (setuid/setgid/sticky),
//! xattrs (including `security.capability`), device nodes, FIFOs, hardlinks,
//! symlinks and nanosecond mtimes. Runs in the guest, as root, on the target
//! filesystem; the host never touches the image's files.
//!
//! The tar is trusted for *content* but not for *placement*: entries that would
//! land outside the target (`..`, or a path through a symlink) are rejected.

use std::collections::{HashMap, HashSet};
use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::{self, Read};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{OpenOptionsExt, symlink};
use std::path::{Component, Path, PathBuf};

use clankerd_proto::rootdisk::UnpackSummary;
use tar::{Archive, Entry, EntryType};

/// Everything applied to an entry after it exists.
#[derive(Debug, Default, Clone)]
struct Meta {
    uid: u32,
    gid: u32,
    mode: u32,
    mtime: (i64, i64),
    xattrs: Vec<(Vec<u8>, Vec<u8>)>,
}

/// With `lenient`, operations only root may do (chown to other owners, device
/// nodes, privileged xattrs) are skipped instead of failing. For development
/// runs without root; the real population boot never sets it.
pub fn unpack<R: Read>(reader: R, target: &Path, lenient: bool) -> io::Result<UnpackSummary> {
    let mut u = Unpacker {
        lenient,
        target: target.to_path_buf(),
        known_dirs: HashSet::new(),
        dir_meta: HashMap::new(),
        dir_order: Vec::new(),
        summary: UnpackSummary::default(),
    };
    u.known_dirs.insert(PathBuf::new());
    let mut archive = Archive::new(reader);
    for entry in archive.entries()? {
        let entry = entry?;
        let name = String::from_utf8_lossy(&entry.path_bytes()).into_owned();
        u.entry(entry)
            .map_err(|e| io::Error::new(e.kind(), format!("{name}: {e}")))?;
    }
    u.finish()?;
    Ok(u.summary)
}

struct Unpacker {
    lenient: bool,
    target: PathBuf,
    /// Relative paths verified (or created by us) to be real directories.
    known_dirs: HashSet<PathBuf>,
    /// Directory metadata is applied last, deepest first, so that creating
    /// children neither fails on restrictive modes nor disturbs mtimes.
    dir_meta: HashMap<PathBuf, Meta>,
    dir_order: Vec<PathBuf>,
    summary: UnpackSummary,
}

impl Unpacker {
    fn entry<R: Read>(&mut self, mut entry: Entry<'_, R>) -> io::Result<()> {
        let ty = entry.header().entry_type();
        match ty {
            EntryType::XGlobalHeader | EntryType::XHeader => return Ok(()),
            EntryType::GNUSparse => return Err(io::Error::other("sparse tar entries unsupported")),
            _ => {}
        }
        let rel = normalize(&entry.path_bytes())?;
        let meta = read_meta(&mut entry)?;
        let abs = self.target.join(&rel);

        if rel.as_os_str().is_empty() {
            // The root entry only carries metadata for the target directory.
            if ty.is_dir() {
                self.remember_dir(rel, meta);
            }
            return Ok(());
        }
        self.ensure_parent(&rel)?;

        match ty {
            EntryType::Directory => {
                match fs::symlink_metadata(&abs) {
                    Ok(m) if m.is_dir() => {}
                    Ok(_) => {
                        fs::remove_file(&abs)?;
                        fs::create_dir(&abs)?;
                    }
                    Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&abs)?,
                    Err(e) => return Err(e),
                }
                self.remember_dir(rel, meta);
            }
            EntryType::Regular | EntryType::Continuous => {
                remove_non_dir(&abs)?;
                let mut file = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .custom_flags(libc::O_NOFOLLOW)
                    .open(&abs)?;
                self.summary.bytes += io::copy(&mut entry, &mut file)?;
                drop(file);
                apply(&abs, &meta, true, self.lenient)?;
            }
            EntryType::Symlink => {
                let link = entry
                    .link_name_bytes()
                    .ok_or_else(|| io::Error::other("symlink without target"))?;
                remove_non_dir(&abs)?;
                symlink(OsStr::from_bytes(&link), &abs)?;
                apply(&abs, &meta, false, self.lenient)?;
            }
            EntryType::Link => {
                let link = entry
                    .link_name_bytes()
                    .ok_or_else(|| io::Error::other("hardlink without target"))?;
                let target_rel = normalize(&link)?;
                self.ensure_parent(&target_rel)?;
                remove_non_dir(&abs)?;
                let (from, to) = (cstr(&self.target.join(target_rel))?, cstr(&abs)?);
                // SAFETY: valid NUL-terminated paths; link() does not follow symlinks.
                check(unsafe { libc::link(from.as_ptr(), to.as_ptr()) })?;
            }
            EntryType::Char | EntryType::Block | EntryType::Fifo => {
                remove_non_dir(&abs)?;
                let (kind, dev) = match ty {
                    EntryType::Fifo => (libc::S_IFIFO, 0),
                    _ => {
                        let major = entry.header().device_major()?.unwrap_or(0);
                        let minor = entry.header().device_minor()?.unwrap_or(0);
                        let kind = if ty == EntryType::Char {
                            libc::S_IFCHR
                        } else {
                            libc::S_IFBLK
                        };
                        (kind, libc::makedev(major, minor))
                    }
                };
                let path = cstr(&abs)?;
                // SAFETY: valid NUL-terminated path.
                let made = check(unsafe { libc::mknod(path.as_ptr(), kind | 0o600, dev) });
                match made {
                    Err(e) if self.lenient && e.raw_os_error() == Some(libc::EPERM) => {
                        self.summary.entries += 1;
                        return Ok(());
                    }
                    other => other?,
                }
                apply(&abs, &meta, true, self.lenient)?;
            }
            other => {
                return Err(io::Error::other(format!(
                    "unsupported tar entry type {other:?}"
                )));
            }
        }
        self.summary.entries += 1;
        Ok(())
    }

    fn remember_dir(&mut self, rel: PathBuf, meta: Meta) {
        if self.dir_meta.insert(rel.clone(), meta).is_none() {
            self.dir_order.push(rel.clone());
        }
        self.known_dirs.insert(rel);
    }

    /// Makes sure every ancestor of `rel` is a real directory, creating missing
    /// ones (their metadata arrives with their own entry, if any).
    fn ensure_parent(&mut self, rel: &Path) -> io::Result<()> {
        let Some(parent) = rel.parent() else {
            return Ok(());
        };
        if self.known_dirs.contains(parent) {
            return Ok(());
        }
        let mut cur = PathBuf::new();
        for comp in parent.components() {
            cur.push(comp);
            if self.known_dirs.contains(&cur) {
                continue;
            }
            let abs = self.target.join(&cur);
            match fs::symlink_metadata(&abs) {
                Ok(m) if m.is_dir() => {}
                Ok(_) => {
                    return Err(io::Error::other(format!(
                        "{} is not a directory (refusing to follow it)",
                        cur.display()
                    )));
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => fs::create_dir(&abs)?,
                Err(e) => return Err(e),
            }
            self.known_dirs.insert(cur.clone());
        }
        Ok(())
    }

    fn finish(&mut self) -> io::Result<()> {
        let mut dirs = std::mem::take(&mut self.dir_order);
        dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
        for rel in dirs {
            let meta = &self.dir_meta[&rel];
            apply(&self.target.join(&rel), meta, true, self.lenient)
                .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", rel.display())))?;
        }
        Ok(())
    }
}

/// Strips `./` and leading `/`, rejects `..`.
fn normalize(raw: &[u8]) -> io::Result<PathBuf> {
    let mut out = PathBuf::new();
    for comp in Path::new(OsStr::from_bytes(raw)).components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir | Component::RootDir => {}
            Component::ParentDir | Component::Prefix(_) => {
                return Err(io::Error::other("path escapes the target directory"));
            }
        }
    }
    Ok(out)
}

fn read_meta<R: Read>(entry: &mut Entry<'_, R>) -> io::Result<Meta> {
    let h = entry.header();
    let mut meta = Meta {
        uid: h.uid()? as u32,
        gid: h.gid()? as u32,
        mode: h.mode()? & 0o7777,
        mtime: (h.mtime()? as i64, 0),
        xattrs: Vec::new(),
    };
    if let Some(exts) = entry.pax_extensions()? {
        for ext in exts {
            let ext = ext?;
            let key = ext.key_bytes();
            let value = ext.value_bytes();
            let text = || String::from_utf8_lossy(value).into_owned();
            match key {
                b"mtime" => meta.mtime = parse_pax_time(&text())?,
                b"uid" => meta.uid = parse_num(&text())?,
                b"gid" => meta.gid = parse_num(&text())?,
                k if k.starts_with(b"SCHILY.xattr.") => meta
                    .xattrs
                    .push((k[b"SCHILY.xattr.".len()..].to_vec(), value.to_vec())),
                _ => {}
            }
        }
    }
    Ok(meta)
}

fn parse_num(s: &str) -> io::Result<u32> {
    s.trim()
        .parse()
        .map_err(|_| io::Error::other(format!("bad pax number {s:?}")))
}

/// Parses a PAX time (`-86400.5`, `1700000000.123456789`) into seconds and
/// nanoseconds, with nanoseconds in `0..1e9` as `timespec` wants.
fn parse_pax_time(s: &str) -> io::Result<(i64, i64)> {
    let bad = || io::Error::other(format!("bad pax time {s:?}"));
    let s = s.trim();
    let (neg, s) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s),
    };
    let (int, frac) = s.split_once('.').unwrap_or((s, ""));
    let secs: i64 = int.parse().map_err(|_| bad())?;
    if !frac.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    let mut digits: String = frac.chars().take(9).collect();
    while digits.len() < 9 {
        digits.push('0');
    }
    let nanos: i64 = digits.parse().map_err(|_| bad())?;
    Ok(match (neg, nanos) {
        (false, n) => (secs, n),
        (true, 0) => (-secs, 0),
        (true, n) => (-secs - 1, 1_000_000_000 - n),
    })
}

/// Order matters: chown clears setuid and file capabilities, so it goes first;
/// xattrs need write access (user.*), so they precede chmod.
fn apply(path: &Path, meta: &Meta, chmod: bool, lenient: bool) -> io::Result<()> {
    let denied = |e: &io::Error| {
        lenient
            && matches!(
                e.raw_os_error(),
                Some(libc::EPERM | libc::ENOTSUP | libc::EACCES)
            )
    };
    let c = cstr(path)?;
    // SAFETY: valid NUL-terminated path.
    if let Err(e) = check(unsafe { libc::lchown(c.as_ptr(), meta.uid, meta.gid) })
        && !denied(&e)
    {
        return Err(e);
    }
    for (name, value) in &meta.xattrs {
        let name = CString::new(name.clone()).map_err(io::Error::other)?;
        // SAFETY: valid strings; value pointer and length describe a live slice.
        let set = check(unsafe {
            libc::lsetxattr(
                c.as_ptr(),
                name.as_ptr(),
                value.as_ptr().cast(),
                value.len(),
                0,
            )
        });
        if let Err(e) = set
            && !denied(&e)
        {
            return Err(io::Error::new(
                e.kind(),
                format!("setting xattr {}: {e}", name.to_string_lossy()),
            ));
        }
    }
    if chmod {
        // SAFETY: valid NUL-terminated path.
        check(unsafe { libc::chmod(c.as_ptr(), meta.mode) })?;
    }
    let times = [
        libc::timespec {
            tv_sec: 0,
            tv_nsec: libc::UTIME_OMIT,
        },
        libc::timespec {
            tv_sec: meta.mtime.0 as _,
            tv_nsec: meta.mtime.1 as _,
        },
    ];
    // SAFETY: valid path and a two-element timespec array.
    check(unsafe {
        libc::utimensat(
            libc::AT_FDCWD,
            c.as_ptr(),
            times.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    })
}

fn remove_non_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => Err(io::Error::other("a directory is in the way")),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

fn cstr(path: &Path) -> io::Result<CString> {
    CString::new(OsString::from(path).into_vec()).map_err(io::Error::other)
}

fn check(rc: libc::c_int) -> io::Result<()> {
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pax_times_split_into_floor_seconds_and_positive_nanos() {
        assert_eq!(
            parse_pax_time("1700000000.5").unwrap(),
            (1_700_000_000, 500_000_000)
        );
        assert_eq!(parse_pax_time("-86400.5").unwrap(), (-86_401, 500_000_000));
        assert_eq!(parse_pax_time("-3").unwrap(), (-3, 0));
        assert_eq!(parse_pax_time("7.000000001999").unwrap(), (7, 1));
        assert!(parse_pax_time("x").is_err());
    }

    #[test]
    fn normalize_strips_prefixes_and_rejects_parent_refs() {
        assert_eq!(normalize(b"./a/b").unwrap(), PathBuf::from("a/b"));
        assert_eq!(normalize(b"/a").unwrap(), PathBuf::from("a"));
        assert_eq!(normalize(b"./").unwrap(), PathBuf::new());
        assert!(normalize(b"a/../b").is_err());
    }
}
