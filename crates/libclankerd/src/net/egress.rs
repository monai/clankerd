//! Validate IPv4 source bounds before a guest frame reaches host PF states.
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
}
