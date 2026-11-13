#![cfg(target_os = "macos")]
use clankerd_network_ipc::{Connection, Request, Settings};
use std::time::{Duration, Instant};

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
        .0;
    assert!(response.connected);
    response.generation.unwrap()
}
fn connected(key: &str, generation: &str) -> bool {
    Connection::connect()
        .unwrap()
        .request(&Request::Status {
            key: key.into(),
            generation: generation.into(),
        })
        .unwrap()
        .0
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
    let (_, socket_a) = owner_a
        .request(&Request::Attach {
            key: key_a.clone(),
            generation: a.clone(),
        })
        .unwrap();
    let (_, socket_b) = owner_b
        .request(&Request::Attach {
            key: key_b.clone(),
            generation: b.clone(),
        })
        .unwrap();
    assert!(socket_a.is_some());
    assert!(socket_b.is_some());
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
