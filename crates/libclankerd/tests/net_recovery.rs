//! Controlled native backend failure at the real Unixgram attachment boundary.
use libclankerd::net::{NetBackend, NetSpec, VmnetBackend, recovery::Relay};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixDatagram;
use std::time::{Duration, Instant};

#[test]
fn a_stable_attachment_recovers_helper_failure_without_changing_network_policy() {
    let directory = tempfile::tempdir().unwrap();
    let helper = directory.path().join("vmnet-helper");
    std::fs::write(&helper, r#"#!/usr/bin/env python3
import json,os,socket,sys
from pathlib import Path
with open(sys.argv[0]+'.args','a') as stream: stream.write(json.dumps(sys.argv[1:])+'\n')
if Path(sys.argv[0]+'.blocked').exists(): sys.exit(4)
path=sys.argv[sys.argv.index('--socket')+1]
sock=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM)
sock.bind(path)
print('{"vmnet_mac_address":"0a:d6:36:c1:ea:f3","vmnet_mtu":1500,"vmnet_max_packet_size":1514}',flush=True)
first,peer=sock.recvfrom(65536,socket.MSG_PEEK)
sock.connect(peer)
if len(first)<64: sock.recv(65536)
while True:
    packet=sock.recv(65536)
    if len(packet)>=64: sock.send(packet)
"#).unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let backend = VmnetBackend::new(&helper);
    let spec = NetSpec {
        machine_id: "abcdef0123456789".into(),
        network_id: None,
        dir: directory.path().into(),
        socket: directory.path().join("backend.net"),
        host_config: libclankerd::HostConfig {
            shared_peer_participation: false,
            ..Default::default()
        },
    };
    backend.start(&spec).unwrap();
    let relay = Relay::start(&spec.dir, &spec.socket).unwrap().unwrap();
    let guest = UnixDatagram::bind(directory.path().join("guest.net")).unwrap();
    guest.connect(relay.socket()).unwrap();
    guest
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let packet = [0x61; 1514];
    guest.send(&packet).unwrap();
    let mut response = [0u8; 1514];
    assert_eq!(guest.recv(&mut response).unwrap(), 1514);
    assert_eq!(response, packet);
    for _ in 0..2 {
        let attempts = std::fs::read_to_string(helper.with_extension("args"))
            .unwrap()
            .lines()
            .count();
        let pid: i32 = std::fs::read_to_string(spec.dir.join("net.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        std::fs::write(helper.with_extension("blocked"), "").unwrap();
        // SAFETY: only this test's native helper group is signalled.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
        let deadline = Instant::now() + Duration::from_secs(3);
        while backend.connected(&spec) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!backend.connected(&spec));
        let deadline = Instant::now() + Duration::from_secs(10);
        while std::fs::read_to_string(helper.with_extension("args"))
            .unwrap()
            .lines()
            .count()
            == attempts
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !backend.connected(&spec),
            "failed replacement must remain unavailable"
        );
        std::fs::remove_file(helper.with_extension("blocked")).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut recovered = false;
        while Instant::now() < deadline {
            let _ = guest.send(&packet);
            if guest.recv(&mut response).is_ok() {
                recovered = response == packet;
                break;
            }
        }
        if !recovered {
            backend.stop(&spec);
        }
        assert!(
            recovered,
            "existing guest endpoint did not recover real packets"
        );
    }
    backend.stop(&spec);
    std::thread::sleep(Duration::from_millis(200));
    assert!(!relay.socket().exists());
    assert!(!spec.dir.join("net.pid").exists());
    drop(relay);
    let invocations: Vec<serde_json::Value> =
        std::fs::read_to_string(helper.with_extension("args"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
    assert!(invocations.len() >= 3);
    assert!(
        invocations
            .iter()
            .all(|arguments| arguments == &invocations[0]),
        "recovery changed network policy: {invocations:?}"
    );
}

#[test]
fn automatic_native_prefix_bounds_the_relay() {
    let directory = tempfile::tempdir().unwrap();
    let helper = directory.path().join("vmnet-helper");
    std::fs::write(&helper, r#"#!/usr/bin/env python3
import socket,sys
path=sys.argv[sys.argv.index('--socket')+1]
sock=socket.socket(socket.AF_UNIX,socket.SOCK_DGRAM);sock.bind(path)
print('{"vmnet_mac_address":"0a:d6:36:c1:ea:f3","vmnet_mtu":1500,"vmnet_max_packet_size":1514,"vmnet_nat66_prefix":"fd42:1::"}',flush=True)
while True:
    packet,peer=sock.recvfrom(65536)
    if len(packet)>=64:sock.sendto(packet,peer)
"#).unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let spec = NetSpec {
        machine_id: "abcdef0123456789".into(),
        network_id: None,
        dir: directory.path().into(),
        socket: directory.path().join("backend.net"),
        host_config: Default::default(),
    };
    let backend = VmnetBackend::new(&helper);
    backend.start(&spec).unwrap();
    let relay = Relay::start(&spec.dir, &spec.socket).unwrap().unwrap();
    let guest = UnixDatagram::bind(directory.path().join("guest.net")).unwrap();
    guest.connect(relay.socket()).unwrap();
    guest
        .set_read_timeout(Some(Duration::from_millis(500)))
        .unwrap();
    let mut frame = [0; 70];
    frame[12..14].copy_from_slice(&[0x86, 0xdd]);
    frame[14] = 0x60;
    frame[18..20].copy_from_slice(&16u16.to_be_bytes());
    frame[20] = 17;
    frame[22..38].copy_from_slice(&"fd42:1::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
    frame[38..54].copy_from_slice(&"fd42:2::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
    guest.send(&frame).unwrap();
    let mut response = [0; 70];
    assert_eq!(guest.recv(&mut response).unwrap(), frame.len());
    assert_eq!(response, frame);
    frame[22..38].copy_from_slice(&"fd42:2::2".parse::<std::net::Ipv6Addr>().unwrap().octets());
    guest.send(&frame).unwrap();
    let error = guest.recv(&mut response).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    backend.stop(&spec);
}
