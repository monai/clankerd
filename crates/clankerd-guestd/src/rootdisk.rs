//! Root-disk methods: `FormatExt4`, `UnpackTar` and `PopulateDisk`.
//!
//! Formatting runs the static `mke2fs` shipped next to guestd in the boot
//! directory. Unpacking is [`crate::unpack`]. Populating a disk composes them:
//! format, mount (attaching a loop device first when the "disk" is a regular
//! file), unpack, sync, unmount.

use std::ffi::CString;
use std::fs::{self, OpenOptions};
use std::io::{self, BufReader};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use clankerd_proto::rootdisk::{
    ERROR_FAILED, ERROR_INVALID_PARAMETER, ErrorDetail, FormatExt4, METHOD_FORMAT_EXT4,
    METHOD_POPULATE_DISK, METHOD_UNPACK_TAR, PopulateDisk, UnpackSummary, UnpackTar,
};
use clankerd_proto::varlink::{self, Call, Reply};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::unpack::unpack;

pub struct Ctx {
    /// Directory holding the static e2fsprogs binaries (default: next to guestd).
    pub boot_dir: PathBuf,
    /// Populate boot: power off (exit) after a successful `PopulateDisk`.
    pub populate_mode: bool,
}

/// Failure of one method: a protocol-level error name plus a message.
struct Failure(&'static str, String);

impl Failure {
    fn invalid(msg: impl Into<String>) -> Self {
        Failure(ERROR_INVALID_PARAMETER, msg.into())
    }
    fn failed(msg: impl Into<String>) -> Self {
        Failure(ERROR_FAILED, msg.into())
    }
}

impl From<io::Error> for Failure {
    fn from(e: io::Error) -> Self {
        Failure::failed(e.to_string())
    }
}

/// Serves `call` if it is a root-disk method; returns whether it did.
pub fn handle(
    call: &Call,
    input: &mut BufReader<UnixStream>,
    out: &mut UnixStream,
    ctx: &Ctx,
) -> bool {
    let result = match call.method.as_str() {
        METHOD_FORMAT_EXT4 => params(call).and_then(|p| format_ext4(ctx, p)),
        METHOD_UNPACK_TAR => params(call).and_then(|p| unpack_tar(input, p)),
        METHOD_POPULATE_DISK => params(call).and_then(|p| populate_disk(ctx, input, p)),
        _ => return false,
    };
    let reply = match &result {
        Ok(v) => Reply {
            parameters: v.clone(),
            continues: false,
            error: None,
        },
        Err(Failure(name, message)) => Reply {
            parameters: serde_json::to_value(ErrorDetail {
                message: message.clone(),
            })
            .unwrap_or(Value::Null),
            continues: false,
            error: Some((*name).into()),
        },
    };
    let _ = varlink::write(out, &reply);
    if result.is_ok() && ctx.populate_mode && call.method == METHOD_POPULATE_DISK {
        // The population boot has done its job: power off.
        std::process::exit(0);
    }
    true
}

fn params<T: DeserializeOwned>(call: &Call) -> Result<T, Failure> {
    serde_json::from_value(call.parameters.clone())
        .map_err(|e| Failure::invalid(format!("bad parameters: {e}")))
}

fn format_ext4(ctx: &Ctx, p: FormatExt4) -> Result<Value, Failure> {
    format_device(ctx, Path::new(&p.device), p.size, p.label.as_deref())?;
    Ok(Value::Object(Default::default()))
}

fn format_device(
    ctx: &Ctx,
    device: &Path,
    size: Option<u64>,
    label: Option<&str>,
) -> Result<(), Failure> {
    let meta =
        fs::metadata(device).map_err(|e| Failure::invalid(format!("{}: {e}", device.display())))?;
    if meta.is_file() {
        if let Some(size) = size {
            OpenOptions::new()
                .write(true)
                .open(device)?
                .set_len(size.max(meta.len()))?;
        }
    } else if !meta.file_type().is_block_device() {
        return Err(Failure::invalid(format!(
            "{} is neither a regular file nor a block device",
            device.display()
        )));
    }
    let mke2fs = ctx.boot_dir.join("mke2fs");
    if !mke2fs.exists() {
        return Err(Failure::failed(format!(
            "{} not found (the boot directory must ship mke2fs)",
            mke2fs.display()
        )));
    }
    let mut cmd = Command::new(&mke2fs);
    // Ignore any mke2fs.conf of the environment: built-in ext4 defaults only.
    cmd.env("MKE2FS_CONFIG", "/nonexistent/mke2fs.conf")
        .args(["-t", "ext4", "-F", "-q", "-m", "0"])
        .args(["-E", "lazy_itable_init=1,nodiscard,root_owner=0:0"]);
    if let Some(label) = label {
        cmd.args(["-L", label]);
    }
    cmd.arg(device);
    let output = cmd
        .output()
        .map_err(|e| Failure::failed(format!("running {}: {e}", mke2fs.display())))?;
    if !output.status.success() {
        return Err(Failure::failed(format!(
            "mke2fs failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(())
}

fn unpack_tar(input: &mut BufReader<UnixStream>, p: UnpackTar) -> Result<Value, Failure> {
    let target = Path::new(&p.target);
    if !target.is_dir() {
        return Err(Failure::invalid(format!(
            "{} is not a directory",
            target.display()
        )));
    }
    summary(unpack_stream(input, target)?)
}

fn unpack_stream(
    input: &mut BufReader<UnixStream>,
    target: &Path,
) -> Result<UnpackSummary, Failure> {
    let summary = unpack(&mut *input, target)?;
    // Consume the end-of-archive padding up to the client's half-close.
    io::copy(input, &mut io::sink())?;
    Ok(summary)
}

fn summary(s: UnpackSummary) -> Result<Value, Failure> {
    Ok(serde_json::to_value(s).unwrap_or(Value::Null))
}

fn populate_disk(
    ctx: &Ctx,
    input: &mut BufReader<UnixStream>,
    p: PopulateDisk,
) -> Result<Value, Failure> {
    let device = Path::new(&p.device);
    format_device(ctx, device, p.size, None)?;

    let loop_dev = if fs::metadata(device)?.is_file() {
        Some(LoopDevice::attach(device)?)
    } else {
        None
    };
    let source = loop_dev
        .as_ref()
        .map_or_else(|| device.to_path_buf(), |l| l.path.clone());

    let mount_point =
        std::env::temp_dir().join(format!("clankerd-populate-{}", std::process::id()));
    fs::create_dir_all(&mount_point)?;
    mount_ext4(&source, &mount_point)?;
    let result = unpack_stream(input, &mount_point);
    // SAFETY: sync has no preconditions.
    unsafe { libc::sync() };
    let umount = umount(&mount_point);
    let _ = fs::remove_dir(&mount_point);
    drop(loop_dev);
    let unpacked = result?;
    umount?;
    summary(unpacked)
}

fn mount_ext4(source: &Path, target: &Path) -> io::Result<()> {
    let src = CString::new(source.as_os_str().as_bytes())?;
    let dst = CString::new(target.as_os_str().as_bytes())?;
    // SAFETY: valid NUL-terminated strings; no data argument.
    let rc = unsafe {
        libc::mount(
            src.as_ptr(),
            dst.as_ptr(),
            c"ext4".as_ptr(),
            0,
            std::ptr::null(),
        )
    };
    if rc < 0 {
        let e = io::Error::last_os_error();
        return Err(io::Error::new(
            e.kind(),
            format!("mounting {}: {e}", source.display()),
        ));
    }
    Ok(())
}

fn umount(target: &Path) -> Result<(), Failure> {
    let dst = CString::new(target.as_os_str().as_bytes()).map_err(io::Error::other)?;
    // SAFETY: valid NUL-terminated string.
    if unsafe { libc::umount2(dst.as_ptr(), 0) } < 0 {
        return Err(Failure::failed(format!(
            "unmounting {}: {}",
            target.display(),
            io::Error::last_os_error()
        )));
    }
    Ok(())
}

/// A loop device attached to a regular file, detached on drop (autoclear covers
/// the case where it is still mounted).
struct LoopDevice {
    path: PathBuf,
}

const LOOP_SET_FD: libc::c_ulong = 0x4C00;
const LOOP_CLR_FD: libc::c_ulong = 0x4C01;
const LOOP_SET_STATUS64: libc::c_ulong = 0x4C04;
const LOOP_CTL_GET_FREE: libc::c_ulong = 0x4C82;
const LO_FLAGS_AUTOCLEAR: u32 = 4;

impl LoopDevice {
    fn attach(file: &Path) -> io::Result<Self> {
        let ctl = OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/loop-control")?;
        // SAFETY: LOOP_CTL_GET_FREE takes no argument and returns a device number.
        let n = unsafe { libc::ioctl(ctl.as_raw_fd(), LOOP_CTL_GET_FREE) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        let path = PathBuf::from(format!("/dev/loop{n}"));
        let dev = OpenOptions::new().read(true).write(true).open(&path)?;
        let backing = OpenOptions::new().read(true).write(true).open(file)?;
        // SAFETY: LOOP_SET_FD takes the backing file descriptor.
        if unsafe { libc::ioctl(dev.as_raw_fd(), LOOP_SET_FD, backing.as_raw_fd()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // struct loop_info64 is 232 bytes; lo_flags sits at offset 52.
        let mut info = [0u8; 232];
        info[52..56].copy_from_slice(&LO_FLAGS_AUTOCLEAR.to_ne_bytes());
        // SAFETY: info is a correctly sized loop_info64.
        if unsafe { libc::ioctl(dev.as_raw_fd(), LOOP_SET_STATUS64, info.as_ptr()) } < 0 {
            let e = io::Error::last_os_error();
            // SAFETY: detaching the device we just attached.
            unsafe { libc::ioctl(dev.as_raw_fd(), LOOP_CLR_FD) };
            return Err(e);
        }
        Ok(LoopDevice { path })
    }
}

impl Drop for LoopDevice {
    fn drop(&mut self) {
        if let Ok(dev) = OpenOptions::new().read(true).write(true).open(&self.path) {
            // SAFETY: detaching our own device; fails harmlessly if already cleared.
            unsafe { libc::ioctl(dev.as_raw_fd(), LOOP_CLR_FD) };
        }
    }
}
