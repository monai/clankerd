//! Interface configuration through the classic ioctls (SIOCSIFADDR, ...), so
//! guestd needs neither netlink nor an `ip` binary. Root and a real interface
//! are required: verified on the M2 only.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::time::Duration;

use crate::poll::poll_until;

fn control_socket() -> io::Result<OwnedFd> {
    // SAFETY: plain socket(2); the fd is owned right away.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

fn ifreq(name: &str) -> io::Result<libc::ifreq> {
    let cname = CString::new(name).map_err(io::Error::other)?;
    let bytes = cname.as_bytes_with_nul();
    // SAFETY: ifreq is plain data; all-zero is valid.
    let mut req: libc::ifreq = unsafe { std::mem::zeroed() };
    if bytes.len() > req.ifr_name.len() {
        return Err(io::Error::other(format!(
            "interface name {name} is too long"
        )));
    }
    for (dst, src) in req.ifr_name.iter_mut().zip(bytes) {
        *dst = *src as libc::c_char;
    }
    Ok(req)
}

fn ioctl<T>(sock: &OwnedFd, request: libc::c_ulong, arg: &mut T, what: &str) -> io::Result<()> {
    // SAFETY: `arg` is the structure the request expects.
    if unsafe { libc::ioctl(sock.as_raw_fd(), request as _, arg as *mut T) } < 0 {
        let e = io::Error::last_os_error();
        return Err(io::Error::new(e.kind(), format!("{what}: {e}")));
    }
    Ok(())
}

/// Waits for the kernel to create `name` (virtio devices probe asynchronously).
pub fn wait_for(name: &str, timeout: Duration) -> io::Result<()> {
    let path = Path::new("/sys/class/net").join(name);
    if poll_until(timeout, Duration::from_millis(50), || path.exists()) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("network interface {name} did not appear (no virtio-net device?)"),
        ))
    }
}

pub fn set_up(name: &str) -> io::Result<()> {
    let sock = control_socket()?;
    let mut req = ifreq(name)?;
    ioctl(
        &sock,
        libc::SIOCGIFFLAGS,
        &mut req,
        &format!("reading flags of {name}"),
    )?;
    // SAFETY: SIOCGIFFLAGS filled the flags member of the union.
    unsafe { req.ifr_ifru.ifru_flags |= (libc::IFF_UP | libc::IFF_RUNNING) as libc::c_short };
    ioctl(
        &sock,
        libc::SIOCSIFFLAGS,
        &mut req,
        &format!("bringing {name} up"),
    )
}
