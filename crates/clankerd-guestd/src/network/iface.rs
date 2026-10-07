//! Interface configuration through the classic ioctls (SIOCSIFADDR, ...), so
//! guestd needs neither netlink nor an `ip` binary. Root and a real interface
//! are required: verified on the M2 only.

use std::ffi::CString;
use std::io;
use std::net::Ipv4Addr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::Path;
use std::time::{Duration, Instant};

use super::dhcp::Lease;

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

fn sockaddr_in(addr: Ipv4Addr) -> libc::sockaddr {
    let sin = libc::sockaddr_in {
        sin_family: libc::AF_INET as libc::sa_family_t,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from(addr).to_be(),
        },
        sin_zero: [0; 8],
    };
    // SAFETY: sockaddr and sockaddr_in have the same size; both are plain data.
    unsafe { std::mem::transmute::<libc::sockaddr_in, libc::sockaddr>(sin) }
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
    let deadline = Instant::now() + timeout;
    while !path.exists() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("network interface {name} did not appear (no virtio-net device?)"),
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// The interface's hardware address as the kernel reports it.
pub fn mac(name: &str) -> io::Result<[u8; 6]> {
    let text = std::fs::read_to_string(Path::new("/sys/class/net").join(name).join("address"))?;
    let mut mac = [0u8; 6];
    let mut parts = text.trim().split(':');
    for byte in &mut mac {
        *byte = parts
            .next()
            .and_then(|p| u8::from_str_radix(p, 16).ok())
            .ok_or_else(|| io::Error::other(format!("unparsable MAC address {:?}", text.trim())))?;
    }
    Ok(mac)
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

/// Installs the lease: address and netmask, MTU, and the default route.
pub fn apply_lease(name: &str, lease: &Lease) -> io::Result<()> {
    let sock = control_socket()?;
    let mask = Ipv4Addr::from(
        u32::MAX
            .checked_shl(32 - u32::from(lease.prefix_len))
            .unwrap_or(0),
    );

    let mut req = ifreq(name)?;
    req.ifr_ifru.ifru_addr = sockaddr_in(lease.address);
    ioctl(
        &sock,
        libc::SIOCSIFADDR,
        &mut req,
        &format!("setting the address of {name}"),
    )?;
    let mut req = ifreq(name)?;
    req.ifr_ifru.ifru_netmask = sockaddr_in(mask);
    ioctl(
        &sock,
        libc::SIOCSIFNETMASK,
        &mut req,
        &format!("setting the netmask of {name}"),
    )?;
    if let Some(mtu) = lease.mtu {
        let mut req = ifreq(name)?;
        req.ifr_ifru.ifru_mtu = i32::from(mtu);
        ioctl(
            &sock,
            libc::SIOCSIFMTU,
            &mut req,
            &format!("setting the MTU of {name}"),
        )?;
    }

    if let Some(router) = lease.router {
        let dev = CString::new(name).map_err(io::Error::other)?;
        // SAFETY: rtentry is plain data; all-zero is valid.
        let mut route: libc::rtentry = unsafe { std::mem::zeroed() };
        route.rt_dst = sockaddr_in(Ipv4Addr::UNSPECIFIED);
        route.rt_genmask = sockaddr_in(Ipv4Addr::UNSPECIFIED);
        route.rt_gateway = sockaddr_in(router);
        route.rt_flags = libc::RTF_UP | libc::RTF_GATEWAY;
        route.rt_dev = dev.as_ptr() as *mut libc::c_char;
        // `dev` outlives the ioctl below.
        ioctl(
            &sock,
            libc::SIOCADDRT,
            &mut route,
            "adding the default route",
        )?;
    }
    Ok(())
}
