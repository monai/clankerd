//! A received echo reply provides local IPv6 evidence on a link without RAs.
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub fn reachable(interface: &str) -> io::Result<bool> {
    let name = CString::new(interface).map_err(io::Error::other)?;
    // SAFETY: a valid interface name and initialized socket data; fd owned below.
    unsafe {
        let index = libc::if_nametoindex(name.as_ptr());
        if index == 0 {
            return Err(io::Error::last_os_error());
        }
        let fd = libc::socket(
            libc::AF_INET6,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::IPPROTO_ICMPV6,
        );
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let socket = OwnedFd::from_raw_fd(fd);
        let timeout = libc::timeval {
            tv_sec: 0,
            tv_usec: 200_000,
        };
        if libc::setsockopt(
            socket.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            &timeout as *const _ as _,
            std::mem::size_of_val(&timeout) as _,
        ) != 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut destination: libc::sockaddr_in6 = std::mem::zeroed();
        destination.sin6_family = libc::AF_INET6 as _;
        destination.sin6_scope_id = index;
        destination.sin6_addr.s6_addr = [0xff, 0x02, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1];
        let mut request = [0u8; 24];
        request[0] = 128;
        // A fresh unpredictable payload keeps unrelated replies from proving connectivity.
        use std::io::Read;
        std::fs::File::open("/dev/urandom")?.read_exact(&mut request[8..])?;
        if libc::sendto(
            socket.as_raw_fd(),
            request.as_ptr() as _,
            request.len(),
            0,
            &destination as *const _ as _,
            std::mem::size_of_val(&destination) as _,
        ) < 0
        {
            return Err(io::Error::last_os_error());
        }
        let mut response = [0u8; 2048];
        let mut source: libc::sockaddr_in6 = std::mem::zeroed();
        let mut size = std::mem::size_of_val(&source) as libc::socklen_t;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(200);
        loop {
            if std::time::Instant::now() >= deadline {
                return Ok(false);
            }
            let count = libc::recvfrom(
                socket.as_raw_fd(),
                response.as_mut_ptr() as _,
                response.len(),
                0,
                &mut source as *mut _ as _,
                &mut size,
            );
            if count < 0 {
                return Ok(false);
            }
            if count as usize == request.len()
                && response[0] == 129
                && response[8..24] == request[8..]
            {
                // Loopback/self-generated multicast replies must not prove a peer exists.
                let local = super::snapshot::read_addresses(interface);
                let peer = std::net::IpAddr::V6(std::net::Ipv6Addr::from(source.sin6_addr.s6_addr));
                if !local.contains(&peer) {
                    return Ok(true);
                }
            }
        }
    }
}
