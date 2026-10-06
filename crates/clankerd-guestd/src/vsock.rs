//! AF_VSOCK helpers. Connections are returned as `UnixStream`: the std type
//! is a plain stream-socket wrapper (read, write, dup, shutdown, timeouts),
//! which is all the rest of guestd uses, so vsock sockets share its code.

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

/// The host's CID.
const VMADDR_CID_HOST: u32 = 2;

fn socket() -> io::Result<OwnedFd> {
    // SAFETY: plain socket call; the fd is wrapped immediately.
    unsafe {
        let fd = libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(OwnedFd::from_raw_fd(fd))
    }
}

fn addr(cid: u32, port: u32) -> libc::sockaddr_vm {
    // SAFETY: sockaddr_vm is plain old data; all-zero is a valid value.
    let mut a: libc::sockaddr_vm = unsafe { std::mem::zeroed() };
    a.svm_family = libc::AF_VSOCK as libc::sa_family_t;
    a.svm_cid = cid;
    a.svm_port = port;
    a
}

/// Listens on `port` for any CID.
pub fn listen(port: u32) -> io::Result<OwnedFd> {
    let fd = socket()?;
    let a = addr(libc::VMADDR_CID_ANY, port);
    // SAFETY: `a` is a valid sockaddr_vm of the given length.
    let rc = unsafe {
        libc::bind(
            fd.as_raw_fd(),
            (&raw const a).cast(),
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    // SAFETY: listen on a bound socket.
    if rc < 0 || unsafe { libc::listen(fd.as_raw_fd(), 64) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

pub fn accept(listener: &OwnedFd) -> Option<UnixStream> {
    // SAFETY: accept on a valid listening fd; the peer address is not needed.
    let c = unsafe {
        libc::accept4(
            listener.as_raw_fd(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            libc::SOCK_CLOEXEC,
        )
    };
    if c < 0 {
        return None;
    }
    // SAFETY: c is a fresh fd we own.
    Some(UnixStream::from(unsafe { OwnedFd::from_raw_fd(c) }))
}

/// Connects to a vsock port on the host.
pub fn connect_host(port: u32) -> io::Result<UnixStream> {
    let fd = socket()?;
    let a = addr(VMADDR_CID_HOST, port);
    // SAFETY: `a` is a valid sockaddr_vm of the given length.
    let rc = unsafe {
        libc::connect(
            fd.as_raw_fd(),
            (&raw const a).cast(),
            std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(UnixStream::from(fd))
}
