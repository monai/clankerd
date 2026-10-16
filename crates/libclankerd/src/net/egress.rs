//! Validate selected IPv4/IPv6 source bounds before a frame reaches host PF states.
use crate::{HostConfig, NetworkMode};

pub(super) fn permits(frame: &[u8], host: &HostConfig) -> bool {
    if host.network_mode == NetworkMode::Bridged {
        return true;
    }
    let mut offset = 14;
    let Some(kind) = frame.get(12..14) else {
        return true; // Native Unixgram connection preamble, not an Ethernet frame.
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
        return if host.network_mode == NetworkMode::HostOnly {
            permits_ipv6(&frame[offset..], None)
        } else {
            host.shared_ipv6_prefix
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
    let prefix = host.shared_network.prefix_len;
    if !(8..=30).contains(&prefix) {
        return false;
    }
    let mask = u32::MAX << (32 - prefix);
    let base = u32::from(host.shared_network.address);
    if source & mask == base && source != base && source != base + 1 && source != (base | !mask) {
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
        let host = HostConfig::default();
        for source in [[192, 168, 105, 2], [192, 168, 105, 250]] {
            assert!(permits(&packet(source), &host));
        }
        for source in [
            [192, 168, 106, 2],
            [192, 168, 105, 1],
            [1, 1, 1, 1],
            [192, 168, 105, 0],
            [192, 168, 105, 255],
        ] {
            assert!(!permits(&packet(source), &host));
        }
    }

    #[test]
    fn source_zero_is_reserved_for_unfragmented_dhcp_broadcast() {
        let host = HostConfig::default();
        let frame = packet([0; 4]);
        assert!(permits(&frame, &host));
        for index in [20, 21, 23, 30, 34, 37] {
            let mut invalid = frame.clone();
            invalid[index] ^= 1;
            assert!(!permits(&invalid, &host), "offset {index}");
        }
    }

    #[test]
    fn vlan_encapsulation_does_not_hide_a_foreign_source() {
        let host = HostConfig::default();
        for source in [[192, 168, 105, 2], [192, 168, 106, 2]] {
            let mut frame = packet(source);
            let expected = permits(&frame, &host);
            frame.splice(12..12, [0x88, 0xa8, 0, 7, 0x81, 0, 0, 8]);
            assert_eq!(permits(&frame, &host), expected);
        }
    }

    #[test]
    fn truncated_ipv4_is_rejected_and_other_attachment_protocols_are_retained() {
        let host = HostConfig::default();
        let frame = packet([192, 168, 105, 2]);
        for len in 14..frame.len() {
            assert!(!permits(&frame[..len], &host));
        }
        for kind in [0x0806u16, 0x86dd] {
            let mut other = frame.clone();
            other[12..14].copy_from_slice(&kind.to_be_bytes());
            assert!(permits(&other, &host));
        }
        assert!(permits(b"VFKT", &host));
        let bridged = HostConfig {
            network_mode: NetworkMode::Bridged,
            ..host
        };
        assert!(permits(&packet([203, 0, 113, 2]), &bridged));
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
        let host = HostConfig {
            shared_ipv6_prefix: Some("fd42:1::".parse().unwrap()),
            ..Default::default()
        };
        for next in [0, 17, 44, 58] {
            assert!(permits(
                &ipv6_packet("fd42:1::2", "fd42:2::2", next, &[0; 32]),
                &host
            ));
            assert!(!permits(
                &ipv6_packet("fd42:2::2", "fd42:3::2", next, &[0; 32]),
                &host
            ));
        }
        assert!(permits(
            &ipv6_packet("fe80::2", "ff02::fb", 17, &[0; 8]),
            &host
        ));
        assert!(!permits(
            &ipv6_packet("::", "fd42:2::2", 17, &[0; 8]),
            &host
        ));
    }

    #[test]
    fn ipv6_dad_router_solicitation_and_mld_bootstrap_survive_source_validation() {
        let host = HostConfig {
            shared_ipv6_prefix: Some("fd42:1::".parse().unwrap()),
            ..Default::default()
        };
        let mut ns = vec![0; 24];
        ns[0] = 135;
        ns[8..24].copy_from_slice(&"fd42:1::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
        assert!(permits(
            &ipv6_packet("::", "ff02::1:ff00:2", 58, &ns),
            &host
        ));
        ns[8..24].copy_from_slice(&"fd42:2::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
        assert!(!permits(
            &ipv6_packet("::", "ff02::1:ff00:2", 58, &ns),
            &host
        ));
        assert!(permits(
            &ipv6_packet("::", "ff02::2", 58, &[133, 0, 0, 0, 0, 0, 0, 0]),
            &host
        ));
        let mut mld = vec![0; 16];
        mld[0] = 58;
        mld[8] = 143;
        assert!(permits(&ipv6_packet("::", "ff02::16", 0, &mld), &host));
        assert!(!permits(&ipv6_packet("::", "ff02::fb", 17, &[0; 8]), &host));
    }
    #[test]
    fn host_only_has_link_local_ipv6_without_foreign_global_sources() {
        let host = HostConfig {
            network_mode: NetworkMode::HostOnly,
            ..Default::default()
        };
        assert!(permits(
            &ipv6_packet("fe80::2", "ff02::fb", 17, &[0; 8]),
            &host
        ));
        assert!(!permits(
            &ipv6_packet("fd42:2::2", "fd42:3::2", 17, &[0; 8]),
            &host
        ));
        let mut ns = vec![0; 24];
        ns[0] = 135;
        ns[8..24].copy_from_slice(&"fe80::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
        assert!(permits(
            &ipv6_packet("::", "ff02::1:ff00:2", 58, &ns),
            &host
        ));
    }
}
