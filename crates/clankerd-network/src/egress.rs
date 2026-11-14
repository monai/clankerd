//! Validate selected IPv4/IPv6 source bounds before a frame reaches host PF states.
use clankerd_network_ipc::Settings;
use std::net::{Ipv4Addr, Ipv6Addr};

pub struct Policy {
    bridged: bool,
    host_only: bool,
    base: u32,
    mask: u32,
    prefix: Option<Ipv6Addr>,
}
impl Policy {
    pub fn new(settings: &Settings) -> Result<Self, String> {
        let (base, bits) = settings
            .subnet
            .split_once('/')
            .ok_or("expected subnet/prefix")?;
        let base = u32::from(base.parse::<Ipv4Addr>().map_err(|e| e.to_string())?);
        let bits: u32 = bits.parse::<u32>().map_err(|e| e.to_string())?;
        if !(8..=30).contains(&bits) || base & (u32::MAX >> bits) != 0 {
            return Err("expected aligned subnet /8 through /30".into());
        }
        Ok(Self {
            bridged: settings.operation_mode == "bridged",
            host_only: settings.operation_mode == "host",
            base,
            mask: u32::MAX << (32 - bits),
            prefix: settings
                .nat66_prefix
                .as_deref()
                .map(str::parse::<Ipv6Addr>)
                .transpose()
                .map_err(|e| e.to_string())?,
        })
    }

    pub fn permits(&self, frame: &[u8]) -> bool {
        if self.bridged {
            return true;
        }
        let mut offset = 14;
        let Some(kind) = frame.get(12..14) else {
            return false;
        };
        let mut kind = u16::from_be_bytes([kind[0], kind[1]]);
        while matches!(kind, 0x8100 | 0x88a8) {
            let Some(tag) = frame.get(offset..offset + 4) else {
                return false;
            };
            kind = u16::from_be_bytes([tag[2], tag[3]]);
            offset += 4;
        }
        if kind == 0x86dd {
            return if self.host_only {
                permits_ipv6(&frame[offset..], None)
            } else {
                self.prefix
                    .is_none_or(|prefix| permits_ipv6(&frame[offset..], Some(prefix)))
            };
        }
        if kind != 0x0800 {
            return true;
        }
        let Some(ip) = frame.get(offset..) else {
            return false;
        };
        if ip.len() < 20 || ip[0] >> 4 != 4 {
            return false;
        }
        let header = usize::from(ip[0] & 15) * 4;
        let length = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
        if header < 20 || length < header || ip.len() < length {
            return false;
        }
        let source = u32::from_be_bytes(ip[12..16].try_into().unwrap());
        let mask = self.mask;
        let base = self.base;
        if source & mask == base && source != base && source != base + 1 && source != (base | !mask)
        {
            return true;
        }
        // Before acquiring an address, DHCP uses 0.0.0.0:68 -> broadcast:67.
        // Fragments cannot use this exception to carry unrelated source-zero traffic.
        source == 0
            && ip[16..20] == [255; 4]
            && ip[9] == 17
            && u16::from_be_bytes([ip[6], ip[7]]) & 0x3fff == 0
            && length >= header + 8
            && ip[header..header + 4] == [0, 68, 0, 67]
    }
}

fn permits_ipv6(ip: &[u8], prefix: Option<std::net::Ipv6Addr>) -> bool {
    if ip.len() < 40 || ip[0] >> 4 != 6 {
        return false;
    }
    let length = 40 + usize::from(u16::from_be_bytes([ip[4], ip[5]]));
    if ip.len() < length {
        return false;
    }
    let source = &ip[8..24];
    let link_local = |address: &[u8]| address[0] == 0xfe && address[1] & 0xc0 == 0x80;
    if prefix.is_some_and(|prefix| source[..8] == prefix.octets()[..8]) || link_local(source) {
        return true;
    }
    if source != [0; 16] || ip[24..26] != [0xff, 2] {
        return false;
    }
    let mut next = ip[6];
    let mut offset = 40;
    // Linux MLD reports carry a hop-by-hop Router Alert header.
    if next == 0 && length >= 48 {
        next = ip[40];
        offset += (usize::from(ip[41]) + 1) * 8;
    }
    if next != 58 || length < offset + 8 {
        return false;
    }
    match ip[offset] {
        131 | 132 | 143 => true, // MLD bootstrap listener reports.
        133 => ip[7] == 255 && ip[26..39] == [0; 13] && ip[39] == 2,
        135 if ip[7] == 255 && length >= offset + 24 => {
            let target = &ip[offset + 8..offset + 24];
            let solicited = [0xff, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0xff];
            ip[24..37] == solicited
                && (prefix.is_some_and(|prefix| target[..8] == prefix.octets()[..8])
                    || link_local(target))
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy {
            bridged: false,
            host_only: false,
            base: u32::from(Ipv4Addr::new(192, 168, 105, 0)),
            mask: 0xffffff00,
            prefix: None,
        }
    }

    fn packet(source: [u8; 4]) -> Vec<u8> {
        let mut frame = vec![0; 42];
        frame[12..14].copy_from_slice(&[8, 0]);
        frame[14] = 0x45;
        frame[16..18].copy_from_slice(&28u16.to_be_bytes());
        frame[23] = 17;
        frame[26..30].copy_from_slice(&source);
        frame[30..34].copy_from_slice(&[255; 4]);
        frame[34..38].copy_from_slice(&[0, 68, 0, 67]);
        frame
    }

    #[test]
    fn selected_network_sources_and_aliases_pass_but_foreign_sources_do_not() {
        let host = policy();
        for source in [[192, 168, 105, 2], [192, 168, 105, 250]] {
            assert!(host.permits(&packet(source)));
        }
        for source in [
            [192, 168, 106, 2],
            [192, 168, 105, 1],
            [1, 1, 1, 1],
            [192, 168, 105, 0],
            [192, 168, 105, 255],
        ] {
            assert!(!host.permits(&packet(source)));
        }
    }

    #[test]
    fn source_zero_is_reserved_for_unfragmented_dhcp_broadcast() {
        let host = policy();
        let frame = packet([0; 4]);
        assert!(host.permits(&frame));
        for index in [20, 21, 23, 30, 34, 37] {
            let mut invalid = frame.clone();
            invalid[index] ^= 1;
            assert!(!host.permits(&invalid), "offset {index}");
        }
    }

    #[test]
    fn vlan_encapsulation_does_not_hide_a_foreign_source() {
        let host = policy();
        for source in [[192, 168, 105, 2], [192, 168, 106, 2]] {
            let mut frame = packet(source);
            let expected = host.permits(&frame);
            frame.splice(12..12, [0x88, 0xa8, 0, 7, 0x81, 0, 0, 8]);
            assert_eq!(host.permits(&frame), expected);
        }
    }

    #[test]
    fn truncated_ipv4_is_rejected_and_other_attachment_protocols_are_retained() {
        let host = policy();
        let frame = packet([192, 168, 105, 2]);
        for len in 14..frame.len() {
            assert!(!host.permits(&frame[..len]));
        }
        for kind in [0x0806u16, 0x86dd] {
            let mut other = frame.clone();
            other[12..14].copy_from_slice(&kind.to_be_bytes());
            assert!(host.permits(&other));
        }
        assert!(!host.permits(b"VFKT"));
        let bridged = Policy {
            bridged: true,
            ..host
        };
        assert!(bridged.permits(&packet([203, 0, 113, 2])));
    }
    fn ipv6_packet(source: &str, destination: &str, next: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0; 54];
        frame[12..14].copy_from_slice(&[0x86, 0xdd]);
        frame[14] = 0x60;
        frame[18..20].copy_from_slice(&(payload.len() as u16).to_be_bytes());
        frame[20] = next;
        frame[21] = 255;
        frame[22..38].copy_from_slice(&source.parse::<std::net::Ipv6Addr>().unwrap().octets());
        frame[38..54].copy_from_slice(&destination.parse::<std::net::Ipv6Addr>().unwrap().octets());
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn explicit_ipv6_prefix_rejects_foreign_sources_with_extensions_and_fragments() {
        let host = Policy {
            prefix: Some("fd42:1::".parse().unwrap()),
            ..policy()
        };
        for next in [0, 17, 44, 58] {
            assert!(host.permits(&ipv6_packet("fd42:1::2", "fd42:2::2", next, &[0; 32])));
            assert!(!host.permits(&ipv6_packet("fd42:2::2", "fd42:3::2", next, &[0; 32])));
        }
        assert!(host.permits(&ipv6_packet("fe80::2", "ff02::fb", 17, &[0; 8])));
        assert!(!host.permits(&ipv6_packet("::", "fd42:2::2", 17, &[0; 8])));
    }

    #[test]
    fn ipv6_dad_router_solicitation_and_mld_bootstrap_survive_source_validation() {
        let host = Policy {
            prefix: Some("fd42:1::".parse().unwrap()),
            ..policy()
        };
        let mut ns = vec![0; 24];
        ns[0] = 135;
        ns[8..24].copy_from_slice(&"fd42:1::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
        assert!(host.permits(&ipv6_packet("::", "ff02::1:ff00:2", 58, &ns)));
        ns[8..24].copy_from_slice(&"fd42:2::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
        assert!(!host.permits(&ipv6_packet("::", "ff02::1:ff00:2", 58, &ns)));
        assert!(host.permits(&ipv6_packet(
            "::",
            "ff02::2",
            58,
            &[133, 0, 0, 0, 0, 0, 0, 0]
        )));
        let mut mld = vec![0; 16];
        mld[0] = 58;
        mld[8] = 143;
        assert!(host.permits(&ipv6_packet("::", "ff02::16", 0, &mld)));
        assert!(!host.permits(&ipv6_packet("::", "ff02::fb", 17, &[0; 8])));
    }
    #[test]
    fn host_only_has_link_local_ipv6_without_foreign_global_sources() {
        let host = Policy {
            host_only: true,
            ..policy()
        };
        assert!(host.permits(&ipv6_packet("fe80::2", "ff02::fb", 17, &[0; 8])));
        assert!(!host.permits(&ipv6_packet("fd42:2::2", "fd42:3::2", 17, &[0; 8])));
        let mut ns = vec![0; 24];
        ns[0] = 135;
        ns[8..24].copy_from_slice(&"fe80::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
        assert!(host.permits(&ipv6_packet("::", "ff02::1:ff00:2", 58, &ns)));
    }
}
