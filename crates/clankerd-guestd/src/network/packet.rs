//! The DHCP [`Transport`] for a guest interface that has no address yet: an
//! AF_PACKET datagram socket bound to the interface, carrying hand-built
//! IPv4/UDP packets (0.0.0.0:68 to 255.255.255.255:67). Receiving at the link
//! layer works whether the server answers by broadcast or by unicast to the
//! offered address, which the kernel's UDP stack would drop.
//!
//! The wrapping functions are unit tested; the socket itself needs root and a
//! real interface and is verified on the M2.

use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

use super::dhcp::Transport;

const ETH_P_IP: u16 = 0x0800;
const CLIENT_PORT: u16 = 68;
const SERVER_PORT: u16 = 67;
const IPPROTO_UDP: u8 = 17;

/// Standard internet checksum (RFC 1071) over `header`.
fn ip_checksum(header: &[u8]) -> u16 {
    let mut sum: u32 = header
        .chunks(2)
        .map(|c| u32::from(u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)])))
        .sum();
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// IPv4 + UDP headers around `payload`, from 0.0.0.0:68 to 255.255.255.255:67.
fn wrap_udp(payload: &[u8]) -> Vec<u8> {
    let udp_len = (8 + payload.len()) as u16;
    let total = 20 + udp_len;
    let mut p = Vec::with_capacity(total as usize);
    p.extend_from_slice(&[0x45, 0x00]);
    p.extend_from_slice(&total.to_be_bytes());
    p.extend_from_slice(&[0, 0, 0x40, 0x00, 64, IPPROTO_UDP, 0, 0]); // id, DF, ttl, proto, checksum
    p.extend_from_slice(&[0, 0, 0, 0]); // source 0.0.0.0
    p.extend_from_slice(&[255, 255, 255, 255]);
    let checksum = ip_checksum(&p[..20]);
    p[10..12].copy_from_slice(&checksum.to_be_bytes());
    p.extend_from_slice(&CLIENT_PORT.to_be_bytes());
    p.extend_from_slice(&SERVER_PORT.to_be_bytes());
    p.extend_from_slice(&udp_len.to_be_bytes());
    p.extend_from_slice(&[0, 0]); // UDP checksum omitted (legal for IPv4)
    p.extend_from_slice(payload);
    p
}

/// The UDP payload of an IPv4 packet addressed to the client port, if it is one.
fn unwrap_udp(packet: &[u8]) -> Option<&[u8]> {
    let ihl = usize::from(*packet.first()? & 0x0f) * 4;
    if packet[0] >> 4 != 4 || ihl < 20 || packet.len() < ihl + 8 || packet[9] != IPPROTO_UDP {
        return None;
    }
    let udp = &packet[ihl..];
    if u16::from_be_bytes([udp[2], udp[3]]) != CLIENT_PORT {
        return None;
    }
    let len = usize::from(u16::from_be_bytes([udp[4], udp[5]]));
    udp.get(8..len.max(8).min(udp.len()))
}

pub struct PacketSocket {
    fd: OwnedFd,
    ifindex: i32,
}

impl PacketSocket {
    pub fn open(interface: &str) -> io::Result<Self> {
        let name = CString::new(interface).map_err(io::Error::other)?;
        // SAFETY: valid NUL-terminated name.
        let ifindex = unsafe { libc::if_nametoindex(name.as_ptr()) } as i32;
        if ifindex == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: plain socket(2); the fd is owned right away.
        let fd = unsafe {
            let raw = libc::socket(
                libc::AF_PACKET,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                i32::from(ETH_P_IP.to_be()),
            );
            if raw < 0 {
                return Err(io::Error::last_os_error());
            }
            OwnedFd::from_raw_fd(raw)
        };
        let addr = Self::addr(ifindex, [0; 6]);
        // SAFETY: addr is a valid sockaddr_ll of the stated size.
        let rc = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                (&addr as *const libc::sockaddr_ll).cast(),
                std::mem::size_of::<libc::sockaddr_ll>() as u32,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(PacketSocket { fd, ifindex })
    }

    fn addr(ifindex: i32, mac: [u8; 6]) -> libc::sockaddr_ll {
        // SAFETY: sockaddr_ll is plain data; all-zero is valid.
        let mut a: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        a.sll_family = libc::AF_PACKET as u16;
        a.sll_protocol = ETH_P_IP.to_be();
        a.sll_ifindex = ifindex;
        a.sll_halen = 6;
        a.sll_addr[..6].copy_from_slice(&mac);
        a
    }
}

impl Transport for PacketSocket {
    fn send(&mut self, payload: &[u8]) -> io::Result<()> {
        let packet = wrap_udp(payload);
        let to = Self::addr(self.ifindex, [0xff; 6]);
        // SAFETY: packet and `to` are valid for the stated lengths.
        let rc = unsafe {
            libc::sendto(
                self.fd.as_raw_fd(),
                packet.as_ptr().cast(),
                packet.len(),
                0,
                (&to as *const libc::sockaddr_ll).cast(),
                std::mem::size_of::<libc::sockaddr_ll>() as u32,
            )
        };
        if rc < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        let mut pfd = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut pfd, 1, ms) };
        if ready < 0 {
            let e = io::Error::last_os_error();
            return if e.kind() == io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(e)
            };
        }
        if ready == 0 {
            return Ok(None);
        }
        let mut buf = [0u8; 2048];
        // SAFETY: buf is valid for its length.
        let n = unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        // Not a DHCP packet: report "nothing yet"; the caller keeps waiting.
        Ok(Some(
            unwrap_udp(&buf[..n as usize]).unwrap_or_default().to_vec(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_ip_checksum_matches_the_textbook_example() {
        // 4500 0073 0000 4000 4011 [....] c0a8 0001 c0a8 00c7 -> b861
        let header = [
            0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0x00, 0x00, 0xc0, 0xa8,
            0x00, 0x01, 0xc0, 0xa8, 0x00, 0xc7,
        ];
        assert_eq!(ip_checksum(&header), 0xb861);
    }

    #[test]
    fn a_wrapped_payload_is_a_valid_broadcast_udp_packet_to_the_server_port() {
        let p = wrap_udp(b"hello");
        assert_eq!(p.len(), 20 + 8 + 5);
        assert_eq!(&p[2..4], &(33u16).to_be_bytes());
        assert_eq!(p[9], 17);
        assert_eq!(&p[12..20], &[0, 0, 0, 0, 255, 255, 255, 255]);
        assert_eq!(ip_checksum(&p[..20]), 0, "header checksum verifies");
        assert_eq!(&p[20..24], &[0, 68, 0, 67]);
        assert_eq!(&p[24..26], &(13u16).to_be_bytes());
        assert_eq!(&p[28..], b"hello");
    }

    /// A server reply as it arrives: 192.168.127.1:67 -> 255.255.255.255:68.
    fn from_server(payload: &[u8], dst_port: u16, proto: u8) -> Vec<u8> {
        let mut p = vec![
            0x45, 0, 0, 0, 0, 0, 0, 0, 64, proto, 0, 0, 192, 168, 127, 1, 255, 255, 255, 255,
        ];
        let udp_len = (8 + payload.len()) as u16;
        p.extend_from_slice(&67u16.to_be_bytes());
        p.extend_from_slice(&dst_port.to_be_bytes());
        p.extend_from_slice(&udp_len.to_be_bytes());
        p.extend_from_slice(&[0, 0]);
        p.extend_from_slice(payload);
        p
    }

    #[test]
    fn the_payload_of_a_packet_for_the_client_port_is_extracted() {
        assert_eq!(
            unwrap_udp(&from_server(b"offer", 68, 17)),
            Some(&b"offer"[..])
        );
    }

    #[test]
    fn other_ports_protocols_and_garbage_are_not_dhcp() {
        assert_eq!(unwrap_udp(&from_server(b"x", 5353, 17)), None);
        assert_eq!(unwrap_udp(&from_server(b"x", 68, 6)), None);
        assert_eq!(unwrap_udp(&[]), None);
        assert_eq!(unwrap_udp(&[0x45, 0, 0]), None);
    }
}
