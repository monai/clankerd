#![cfg(target_os = "macos")]
use clankerd_network_ipc::{Connection, Request, Settings};
use std::{
    os::unix::net::UnixDatagram,
    time::{Duration, Instant},
};

fn create(key: &str, identity: &str) -> String {
    let response = Connection::connect()
        .unwrap()
        .request(&Request::Create {
            key: key.into(),
            settings: Settings {
                interface_id: identity.into(),
                operation_mode: "host".into(),
                start_address: Some("192.168.231.1".into()),
                end_address: Some("192.168.231.254".into()),
                subnet_mask: Some("255.255.255.0".into()),
                nat66_prefix: None,
                shared_interface: None,
                network_id: None,
                subnet: "192.168.231.0/24".into(),
                firewall: false,
            },
        })
        .unwrap()
        .response;
    assert!(response.connected);
    response.generation.unwrap()
}
// Exercise the real vmnet notification and packet path without booting a VM.
fn assert_gateway_arp(socket: &UnixDatagram) {
    socket.set_nonblocking(false).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mac = [2, 0, 0, 0, 0, 2];
    let mut request = vec![0u8; 42];
    request[..6].fill(255);
    request[6..12].copy_from_slice(&mac);
    request[12..22].copy_from_slice(&[8, 6, 0, 1, 8, 0, 6, 4, 0, 1]);
    request[22..28].copy_from_slice(&mac);
    request[28..32].copy_from_slice(&[192, 168, 231, 2]);
    request[38..42].copy_from_slice(&[192, 168, 231, 1]);
    socket.send(&request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut response = [0u8; 65536];
    loop {
        assert!(
            Instant::now() < deadline,
            "gateway did not answer ARP through the packet socket"
        );
        match socket.recv(&mut response) {
            Ok(size)
                if size >= 42
                    && response[12..14] == [8, 6]
                    && response[20..22] == [0, 2]
                    && response[28..32] == [192, 168, 231, 1]
                    && response[38..42] == [192, 168, 231, 2] =>
            {
                break;
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => panic!("packet receive: {error}"),
        }
    }
}

fn connected(key: &str, generation: &str) -> bool {
    Connection::connect()
        .unwrap()
        .request(&Request::Status {
            key: key.into(),
            generation: generation.into(),
        })
        .unwrap()
        .response
        .connected
}
#[test]
#[ignore = "requires the installed macOS network LaunchDaemon"]
fn two_interfaces_share_daemon_and_disconnect_releases_only_its_interface() {
    let key_a = format!("/tmp/clankerd-ipc-test-{}-a", std::process::id());
    let key_b = format!("/tmp/clankerd-ipc-test-{}-b", std::process::id());
    let a = create(&key_a, "c1a00000-0000-4000-8000-000000000001");
    let b = create(&key_b, "c1a00000-0000-4000-8000-000000000002");
    let owner_a = Connection::connect().unwrap();
    let owner_b = Connection::connect().unwrap();
    let reply_a = owner_a
        .request(&Request::Attach {
            key: key_a.clone(),
            generation: a.clone(),
        })
        .unwrap();
    let reply_b = owner_b
        .request(&Request::Attach {
            key: key_b.clone(),
            generation: b.clone(),
        })
        .unwrap();
    let packet_a = UnixDatagram::from(reply_a.packet.unwrap());
    assert_gateway_arp(&packet_a);
    assert!(reply_b.packet.is_some());
    assert!(
        owner_b
            .request(&Request::Attach {
                key: key_b.clone(),
                generation: b.clone()
            })
            .is_err()
    );
    assert!(!connected(&key_b, "old-generation"));
    drop(owner_a);
    let deadline = Instant::now() + Duration::from_secs(10);
    while connected(&key_a, &a) {
        assert!(
            Instant::now() < deadline,
            "disconnected owner retained interface"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(connected(&key_b, &b));
    Connection::connect()
        .unwrap()
        .request(&Request::Stop {
            key: key_b.clone(),
            generation: "old-generation".into(),
        })
        .unwrap();
    assert!(connected(&key_b, &b));
    owner_b
        .request(&Request::Stop {
            key: key_b.clone(),
            generation: b.clone(),
        })
        .unwrap();
    assert!(!connected(&key_b, &b));
}

#[test]
#[ignore = "requires the installed macOS network LaunchDaemon; observes the 120-second handoff deadline"]
fn abandoned_creation_expires_and_stale_stop_cannot_remove_replacement() {
    let key = format!("/tmp/clankerd-ipc-expiry-{}", std::process::id());
    let identity = "c1a00000-0000-4000-8000-000000000003";
    let abandoned = create(&key, identity);
    assert!(connected(&key, &abandoned));
    let deadline = Instant::now() + Duration::from_secs(135);
    while connected(&key, &abandoned) {
        assert!(
            Instant::now() < deadline,
            "unattached interface did not expire"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let client = Connection::connect().unwrap();
    client
        .request(&Request::Stop {
            key: key.clone(),
            generation: abandoned.clone(),
        })
        .unwrap();
    let replacement = create(&key, identity);
    client
        .request(&Request::Stop {
            key: key.clone(),
            generation: abandoned,
        })
        .unwrap();
    assert!(connected(&key, &replacement));
    let owner = Connection::connect().unwrap();
    let reply = owner
        .request(&Request::Attach {
            key: key.clone(),
            generation: replacement.clone(),
        })
        .unwrap();
    let packet = UnixDatagram::from(reply.packet.unwrap());
    assert_gateway_arp(&packet);
    client
        .request(&Request::Stop {
            key: key.clone(),
            generation: replacement.clone(),
        })
        .unwrap();
    assert!(!connected(&key, &replacement));
}
