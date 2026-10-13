//! Configure the guest NIC before workload launch. Interface addresses are
//! inspected independently over vsock; networking requires no image firewall.

pub mod dhcp;
pub mod files;
mod iface;
mod packet;

use std::ffi::CString;
use std::io;
use std::net::Ipv4Addr;
use std::path::Path;
use std::time::Duration;

use clankerd_proto::guest::NetworkConfig;

use dhcp::Lease;

const INTERFACE_WAIT: Duration = Duration::from_secs(10);

/// Forwarding sysctls and the loopback interface. Best effort: failures are logged.
pub fn prepare() {
    for e in files::write_sysctls(Path::new("/proc/sys"), &files::FORWARDING_SYSCTLS) {
        eprintln!("clankerd-guestd: {e}");
    }
    if let Err(e) = iface::set_up("lo") {
        eprintln!("clankerd-guestd: bringing lo up: {e}");
    }
}

/// Configures the NIC. An `Err` means the machine has no (safe) network.
pub fn setup(cfg: &NetworkConfig) -> Result<(), String> {
    if let Err(e) = set_hostname(&cfg.hostname) {
        eprintln!("clankerd-guestd: setting the hostname: {e}");
    }
    iface::wait_for(&cfg.interface, INTERFACE_WAIT).map_err(|e| e.to_string())?;
    iface::set_up(&cfg.interface).map_err(|e| e.to_string())?;
    let mac = iface::mac(&cfg.interface).map_err(|e| format!("reading the MAC address: {e}"))?;
    let mut socket = packet::PacketSocket::open(&cfg.interface)
        .map_err(|e| format!("opening a packet socket on {}: {e}", cfg.interface))?;
    let settings = dhcp::Settings {
        hostname: cfg.hostname.clone(),
        ..Default::default()
    };
    let lease = dhcp::acquire(&mut socket, mac, random_xid(), &settings)?;
    iface::apply_lease(&cfg.interface, &lease)
        .map_err(|e| format!("configuring {}: {e}", cfg.interface))?;
    eprintln!(
        "clankerd-guestd: {} is {}/{} via {}, dns {:?}",
        cfg.interface,
        lease.address,
        lease.prefix_len,
        lease
            .router
            .map_or_else(|| "no router".to_owned(), |r| r.to_string()),
        lease.dns
    );
    files::write_resolv_conf(Path::new("/"), &nameservers(&lease), &lease.search)
        .map_err(|e| format!("writing /etc/resolv.conf: {e}"))?;
    Ok(())
}

/// The lease's DNS servers; the router when the server named none.
fn nameservers(lease: &Lease) -> Vec<Ipv4Addr> {
    if lease.dns.is_empty() {
        lease.router.into_iter().collect()
    } else {
        lease.dns.clone()
    }
}

fn set_hostname(name: &str) -> io::Result<()> {
    let cname = CString::new(name).map_err(io::Error::other)?;
    // SAFETY: valid buffer and length.
    if unsafe { libc::sethostname(cname.as_ptr(), name.len()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    files::write_hostname(Path::new("/"), name)
}

fn random_xid() -> u32 {
    use std::io::Read;
    let mut b = [0u8; 4];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_ok()
    {
        return u32::from_be_bytes(b);
    }
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    t.subsec_nanos() ^ std::process::id()
}

/// Read live interface addresses rather than retaining the startup lease.
pub fn status(interface: &str) -> clankerd_proto::guest::NetworkStatus {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
    let mut status = clankerd_proto::guest::NetworkStatus::default();
    let mut head = std::ptr::null_mut();
    // SAFETY: getifaddrs allocates a linked list valid until freeifaddrs.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return status;
        }
        let mut current = head;
        while !current.is_null() {
            let item = &*current;
            if !item.ifa_name.is_null()
                && !item.ifa_addr.is_null()
                && std::ffi::CStr::from_ptr(item.ifa_name).to_bytes() == interface.as_bytes()
            {
                let address = match (*item.ifa_addr).sa_family as i32 {
                    libc::AF_INET => {
                        let addr = &*(item.ifa_addr as *const libc::sockaddr_in);
                        Some(IpAddr::V4(Ipv4Addr::from(
                            addr.sin_addr.s_addr.to_ne_bytes(),
                        )))
                    }
                    libc::AF_INET6 => {
                        let addr = &*(item.ifa_addr as *const libc::sockaddr_in6);
                        Some(IpAddr::V6(Ipv6Addr::from(addr.sin6_addr.s6_addr)))
                    }
                    _ => None,
                };
                if let Some(address) = address {
                    status.addresses.push(address);
                }
            }
            current = item.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    status
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lease(dns: Vec<Ipv4Addr>, router: Option<Ipv4Addr>) -> Lease {
        Lease {
            address: Ipv4Addr::new(192, 168, 127, 2),
            prefix_len: 24,
            router,
            dns,
            search: Vec::new(),
            mtu: None,
            lease_secs: None,
            server: Ipv4Addr::new(192, 168, 127, 1),
        }
    }

    #[test]
    fn dns_comes_from_the_lease_or_falls_back_to_the_router() {
        let gw = Ipv4Addr::new(192, 168, 127, 1);
        assert_eq!(
            nameservers(&lease(vec![Ipv4Addr::new(9, 9, 9, 9)], Some(gw))),
            [Ipv4Addr::new(9, 9, 9, 9)]
        );
        assert_eq!(nameservers(&lease(vec![], Some(gw))), [gw]);
        assert!(nameservers(&lease(vec![], None)).is_empty());
    }
}
