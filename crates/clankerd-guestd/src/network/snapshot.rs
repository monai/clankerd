use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::Path;

use clankerd_proto::guest::{
    ExternalConnectivity, FamilyConnectivity, NetworkAddress, NetworkRoute, NetworkStatus,
};

pub fn read(interface: &str) -> NetworkStatus {
    let mut status = NetworkStatus::default();
    let ipv6 = fs::read_to_string("/proc/net/if_inet6").unwrap_or_default();
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
                let detail = match (*item.ifa_addr).sa_family as i32 {
                    libc::AF_INET => {
                        let addr = &*(item.ifa_addr as *const libc::sockaddr_in);
                        let prefix = if item.ifa_netmask.is_null() {
                            0
                        } else {
                            (*(item.ifa_netmask as *const libc::sockaddr_in))
                                .sin_addr
                                .s_addr
                                .count_ones() as u8
                        };
                        Some(NetworkAddress {
                            address: IpAddr::V4(Ipv4Addr::from(addr.sin_addr.s_addr.to_ne_bytes())),
                            prefix_len: prefix,
                            interface: interface.into(),
                            scope_id: 0,
                            tentative: false,
                            dad_failed: false,
                            deprecated: false,
                        })
                    }
                    libc::AF_INET6 => {
                        let addr = &*(item.ifa_addr as *const libc::sockaddr_in6);
                        let address = Ipv6Addr::from(addr.sin6_addr.s6_addr);
                        ipv6.lines().find_map(|line| {
                            let fields: Vec<_> = line.split_whitespace().collect();
                            if fields.len() != 6
                                || fields[5] != interface
                                || ipv6_hex(fields[0])? != address
                            {
                                return None;
                            }
                            let flags = u32::from_str_radix(fields[4], 16).ok()?;
                            Some(NetworkAddress {
                                address: IpAddr::V6(address),
                                prefix_len: u8::from_str_radix(fields[2], 16).ok()?,
                                interface: interface.into(),
                                scope_id: addr.sin6_scope_id,
                                tentative: flags & 0x40 != 0,
                                dad_failed: flags & 0x08 != 0,
                                deprecated: flags & 0x20 != 0,
                            })
                        })
                    }
                    _ => None,
                };
                if let Some(detail) = detail {
                    status.addresses.push(detail.address);
                    status.address_details.push(detail);
                }
            }
            current = item.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    for line in fs::read_to_string("/proc/net/route")
        .unwrap_or_default()
        .lines()
        .skip(1)
    {
        let f: Vec<_> = line.split_whitespace().collect();
        if f.len() < 8 || f[0] != interface {
            continue;
        }
        let (Ok(dst), Ok(gateway), Ok(flags), Ok(metric), Ok(mask)) = (
            u32::from_str_radix(f[1], 16),
            u32::from_str_radix(f[2], 16),
            u32::from_str_radix(f[3], 16),
            f[6].parse(),
            u32::from_str_radix(f[7], 16),
        ) else {
            continue;
        };
        if flags & 1 == 0 || flags & 0x200 != 0 {
            continue;
        }
        status.routes.push(NetworkRoute {
            destination: IpAddr::V4(Ipv4Addr::from(dst.to_ne_bytes())),
            prefix_len: mask.count_ones() as u8,
            gateway: (gateway != 0).then(|| IpAddr::V4(Ipv4Addr::from(gateway.to_ne_bytes()))),
            interface: interface.into(),
            metric,
        });
    }
    for line in fs::read_to_string("/proc/net/ipv6_route")
        .unwrap_or_default()
        .lines()
    {
        let f: Vec<_> = line.split_whitespace().collect();
        if f.len() != 10 || f[9] != interface {
            continue;
        }
        let (Some(dst), Some(gateway), Ok(prefix), Ok(metric), Ok(flags)) = (
            ipv6_hex(f[0]),
            ipv6_hex(f[4]),
            u8::from_str_radix(f[1], 16),
            u32::from_str_radix(f[5], 16),
            u32::from_str_radix(f[8], 16),
        ) else {
            continue;
        };
        if flags & 1 == 0 || flags & 0x200 != 0 {
            continue;
        }
        status.routes.push(NetworkRoute {
            destination: IpAddr::V6(dst),
            prefix_len: prefix,
            gateway: (!gateway.is_unspecified()).then_some(IpAddr::V6(gateway)),
            interface: interface.into(),
            metric,
        });
    }
    for line in fs::read_to_string("/etc/resolv.conf")
        .unwrap_or_default()
        .lines()
    {
        let mut words = line.split_whitespace();
        match words.next() {
            Some("nameserver") => {
                if let Some(server) = words.next() {
                    status.nameservers.push(server.into());
                }
            }
            Some("search") => status.search.extend(words.map(String::from)),
            _ => {}
        }
    }
    let carrier = fs::read_to_string(Path::new("/sys/class/net").join(interface).join("carrier"))
        .is_ok_and(|value| value.trim() == "1");
    let running = super::client_running();
    let evidence4 = super::hook::evidence("dhcp");
    let evidence6 =
        super::hook::evidence("dhcp6") || super::hook::evidence("ra") || super::ipv6_peer_seen();
    let family = |v4: bool, evidence: bool| {
        let local_available = running
            && carrier
            && evidence
            && status.address_details.iter().any(|a| {
                a.address.is_ipv4() == v4
                    && !a.tentative
                    && !a.dad_failed
                    && !a.address.is_loopback()
                    && !a.address.is_unspecified()
            });
        let default_route = status
            .routes
            .iter()
            .any(|r| r.destination.is_ipv4() == v4 && r.prefix_len == 0);
        FamilyConnectivity {
            local_available,
            default_route,
            external: if local_available && default_route {
                ExternalConnectivity::Unverified
            } else {
                ExternalConnectivity::Unavailable
            },
        }
    };
    status.ipv4 = family(true, evidence4);
    status.ipv6 = family(false, evidence6);
    status.ready = (status.ipv4.local_available || status.ipv6.local_available)
        && super::hook::resolver_healthy();
    status.diagnostic = if !running {
        "automatic configuration client is not running"
    } else if !carrier {
        "interface carrier is down"
    } else if !super::hook::resolver_healthy() {
        "automatic resolver update failed"
    } else if !status.ready {
        "waiting for a usable address and received network configuration/peer packet"
    } else {
        "local connectivity configured; external destinations require independent verification"
    }
    .into();
    status
}

fn ipv6_hex(text: &str) -> Option<Ipv6Addr> {
    if text.len() != 32 {
        return None;
    }
    let mut bytes = [0; 16];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).ok()?;
    }
    Some(Ipv6Addr::from(bytes))
}

pub fn read_addresses(interface: &str) -> Vec<IpAddr> {
    read(interface).addresses
}
