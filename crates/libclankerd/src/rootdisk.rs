//! Root disks: building the cached base ext4 image of an OCI image and cloning
//! it per machine.
//!
//! The host never formats or unpacks anything itself. A [`DiskPopulator`] boots
//! clankerd-guestd against a blank disk (a *population boot*), streams it the
//! merged tar over the protocol and lets guestd format the disk with the
//! shipped static mke2fs and unpack the tar with real Linux semantics.
//!
//! * [`LocalGuestdPopulator`]: the Linux stand-in; guestd runs as a local
//!   process (as root, since it mounts) with a unix socket for vsock.
//! * The libkrun-backed populator arrives with the VMM ticket.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use clankerd_proto::rootdisk::{ERROR_FAILED, METHOD_POPULATE_DISK, PopulateDisk};
use clankerd_proto::varlink::{self, Call, Reply};

use crate::error::{Error, Result};
use crate::images::{ImageInfo, ImageStore};
use crate::merge::merge_layers;

/// Turns a blank disk file into a formatted, populated ext4 root disk.
pub trait DiskPopulator: Send + Sync {
    /// `disk` is an existing (empty) file; the populator makes it `size` bytes,
    /// formats it and unpacks the tar read from `tar` onto it.
    fn populate(&self, disk: &Path, size: u64, tar: &mut dyn Read) -> Result<()>;
}

/// Runs guestd as a local process in `--populate` mode. Needs root (mount).
#[derive(Debug, Clone)]
pub struct LocalGuestdPopulator {
    guestd: PathBuf,
    boot_dir: PathBuf,
}

impl LocalGuestdPopulator {
    /// `boot_dir` holds the static e2fsprogs binaries (mke2fs).
    pub fn new(guestd: impl Into<PathBuf>, boot_dir: impl Into<PathBuf>) -> Self {
        LocalGuestdPopulator {
            guestd: guestd.into(),
            boot_dir: boot_dir.into(),
        }
    }
}

impl DiskPopulator for LocalGuestdPopulator {
    fn populate(&self, disk: &Path, size: u64, tar: &mut dyn Read) -> Result<()> {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } != 0 {
            return Err(Error::unavailable(
                "populating a root disk locally needs root (guestd mounts the disk); \
                 run as root or use a VM backend",
            ));
        }
        let dir = tempfile::Builder::new().prefix("clankerd-pop").tempdir()?;
        let socket = dir.path().join("guest.sock");
        let listener = UnixListener::bind(&socket)?;
        let fd = listener.as_raw_fd();
        let mut cmd = Command::new(&self.guestd);
        cmd.arg("--populate")
            .arg("--boot-dir")
            .arg(&self.boot_dir)
            .env("LISTEN_FDS", "1")
            .stdin(Stdio::null());
        // SAFETY: only async-signal-safe calls (dup2, fcntl) between fork and exec.
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
        let mut child = cmd.spawn().map_err(|e| {
            Error::unavailable(format!("cannot start {}: {e}", self.guestd.display()))
        })?;
        let result = populate_over(&socket, disk, size, tar);
        if result.is_err() {
            let _ = child.kill();
        }
        let _ = child.wait();
        result
    }
}

/// The host side of `PopulateDisk`: call, stream the tar, half-close, read the reply.
pub(crate) fn populate_over(
    socket: &Path,
    disk: &Path,
    size: u64,
    tar: &mut dyn Read,
) -> Result<()> {
    let mut conn = UnixStream::connect(socket)?;
    varlink::write(
        &mut conn,
        &Call {
            method: METHOD_POPULATE_DISK.into(),
            parameters: serde_json::to_value(PopulateDisk {
                device: disk.to_string_lossy().into_owned(),
                size: Some(size),
            })?,
            more: false,
            upgrade: false,
        },
    )?;
    // An early error reply closes the stream under us; the reply below says why.
    let _ = std::io::copy(tar, &mut conn);
    conn.shutdown(std::net::Shutdown::Write)?;
    let reply: Reply = varlink::read(&mut BufReader::new(conn))?
        .ok_or_else(|| Error::unavailable("guestd closed the connection without replying"))?;
    match reply.error.as_deref() {
        None => Ok(()),
        Some(name) => {
            let message = reply.parameters["message"]
                .as_str()
                .unwrap_or("")
                .to_owned();
            let text = format!("populating the root disk: {name}: {message}");
            Err(if name == ERROR_FAILED {
                Error::system(text)
            } else {
                Error::invalid_parameter(text)
            })
        }
    }
}

/// Root disk size for a merged tar of `tar_len` bytes: room for the contents,
/// ext4 overhead and some headroom. The disk is sparse, so this costs nothing
/// until it is used.
pub(crate) fn base_disk_size(tar_len: u64) -> u64 {
    const MIB: u64 = 1 << 20;
    const GIB: u64 = 1 << 30;
    (tar_len + tar_len / 2 + GIB).div_ceil(MIB) * MIB
}

/// Returns the cached base disk of `image`, building it first if missing.
/// The returned flag says whether this call built it.
pub(crate) fn ensure_base(
    store: &ImageStore,
    populator: &dyn DiskPopulator,
    image: &ImageInfo,
) -> Result<(PathBuf, bool)> {
    let base = store.base_path(&image.id);
    if base.exists() {
        return Ok((base, false));
    }
    let scratch = store.tmp_dir();
    let tag = format!(
        "{}.{}",
        image.id.trim_start_matches("sha256:"),
        std::process::id()
    );
    let tar_path = scratch.join(format!("{tag}.tar"));
    let part = scratch.join(format!("{tag}.ext4"));
    let built = (|| -> Result<()> {
        merge_layers(store, image, &tar_path)?;
        let tar_len = fs::metadata(&tar_path)?.len();
        File::create(&part)?;
        populator.populate(&part, base_disk_size(tar_len), &mut File::open(&tar_path)?)?;
        fs::rename(&part, &base)?;
        Ok(())
    })();
    let _ = fs::remove_file(&tar_path);
    let _ = fs::remove_file(&part);
    built?;
    Ok((base, true))
}

/// Copy-on-write clone where the filesystem has one (APFS `clonefile`, reflink
/// on btrfs/xfs), otherwise a sparse-preserving copy.
pub(crate) fn clone_file(src: &Path, dst: &Path) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let (s, d) = (
            std::ffi::CString::new(src.as_os_str().as_bytes()).map_err(std::io::Error::other)?,
            std::ffi::CString::new(dst.as_os_str().as_bytes()).map_err(std::io::Error::other)?,
        );
        // SAFETY: valid NUL-terminated paths.
        if unsafe { libc::clonefile(s.as_ptr(), d.as_ptr(), 0) } == 0 {
            return Ok(());
        }
    }
    let input = File::open(src)?;
    let output = File::create(dst)?;
    #[cfg(target_os = "linux")]
    {
        const FICLONE: libc::Ioctl = 0x4004_9409 as libc::Ioctl;
        // SAFETY: FICLONE takes the source file descriptor.
        if unsafe { libc::ioctl(output.as_raw_fd(), FICLONE, input.as_raw_fd()) } == 0 {
            return Ok(());
        }
    }
    sparse_copy(&input, &output)?;
    Ok(())
}

fn sparse_copy(input: &File, output: &File) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    let len = input.metadata()?.len();
    output.set_len(len)?;
    let (ifd, mut pos) = (input.as_raw_fd(), 0i64);
    let mut buf = vec![0u8; 1 << 20];
    while (pos as u64) < len {
        // SAFETY: lseek on a valid descriptor.
        let data = unsafe { libc::lseek(ifd, pos, libc::SEEK_DATA) };
        if data < 0 {
            if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENXIO) {
                break; // only a hole remains
            }
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: as above.
        let hole = unsafe { libc::lseek(ifd, data, libc::SEEK_HOLE) };
        let end = if hole < 0 { len as i64 } else { hole };
        let mut at = data;
        while at < end {
            let want = ((end - at) as usize).min(buf.len());
            let n = input.read_at(&mut buf[..want], at as u64)?;
            if n == 0 {
                break;
            }
            output.write_all_at(&buf[..n], at as u64)?;
            at += n as i64;
        }
        pos = end;
    }
    Ok(())
}
