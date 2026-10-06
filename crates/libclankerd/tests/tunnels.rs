//! Seam A: published ports, host-gateway ports and socket bindings, through the
//! public library API only. The stand-in VMM runs guestd as a local process whose
//! "guest loopback" is 127.0.0.2, so guest services listen there and host
//! services on 127.0.0.1 never collide.

mod common;

use std::io::{Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::time::{Duration, Instant};

use common::*;
use libclankerd::vmm::LocalProcessVmm;
use libclankerd::{ErrorKind, HostConfig, Machine, PortBinding};

fn running(env: &Env) -> Machine {
    let engine = env.engine();
    let m = create(&engine, "t", "sleep 60");
    m.start().unwrap();
    m
}

/// A guest-side service: reads until EOF, then answers with what it saw.
/// The answer only comes after the client's half-close.
fn guest_service() -> u16 {
    let l = TcpListener::bind((LocalProcessVmm::GUEST_LOOPBACK, 0)).unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut conn = conn;
                let mut seen = Vec::new();
                conn.read_to_end(&mut seen).unwrap();
                write!(conn, "got {} bytes: ", seen.len()).unwrap();
                conn.write_all(&seen).unwrap();
            });
        }
    });
    port
}

fn talk(addr: SocketAddr, msg: &[u8]) -> String {
    let mut c = TcpStream::connect(addr).unwrap();
    c.write_all(msg).unwrap();
    c.shutdown(Shutdown::Write).unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    out
}

fn refuses(addr: SocketAddr) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_err() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[test]
fn guest_service_is_reachable_through_a_published_port_with_half_close() {
    let env = Env::new();
    let m = running(&env);
    let guest_port = guest_service();

    let published = m.publish(PortBinding::loopback(0, guest_port)).unwrap();
    let addr = published.local_addr();
    assert!(addr.ip().is_loopback());

    assert_eq!(talk(addr, b"hello"), "got 5 bytes: hello");
    let big = vec![b'x'; 1 << 20];
    assert_eq!(
        talk(addr, &big).len(),
        "got 1048576 bytes: ".len() + big.len()
    );
    m.remove(true).unwrap();
}

#[test]
fn dropping_the_handle_closes_the_listener_and_publish_works_again() {
    let env = Env::new();
    let m = running(&env);
    let guest_port = guest_service();

    let first = m.publish(PortBinding::loopback(0, guest_port)).unwrap();
    let addr = first.local_addr();
    assert_eq!(talk(addr, b"a"), "got 1 bytes: a");
    drop(first);
    assert!(refuses(addr), "listener must be closed after drop");

    let second = m.publish(PortBinding::loopback(0, guest_port)).unwrap();
    assert_eq!(talk(second.local_addr(), b"bb"), "got 2 bytes: bb");
    m.remove(true).unwrap();
}

#[test]
fn publishing_needs_a_running_machine() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "idle", "sleep 60");
    let err = m.publish(PortBinding::loopback(0, 80)).err().unwrap();
    assert_eq!(err.kind(), ErrorKind::Conflict);
}

#[test]
fn published_ports_never_bind_to_non_loopback_addresses() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "lo", "sleep 60");
    m.start().unwrap();
    for ip in ["0.0.0.0", "::", "192.0.2.1"] {
        let b = PortBinding {
            host_ip: Some(ip.parse().unwrap()),
            host_port: 0,
            guest_port: 80,
        };
        let err = m.publish(b).err().unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidParameter, "{ip}");

        let host = HostConfig {
            port_bindings: vec![b],
            ..Default::default()
        };
        let err = engine.create(None, sh("sleep 1"), host).err().unwrap();
        assert_eq!(err.kind(), ErrorKind::InvalidParameter, "{ip}");
    }
    // An explicit loopback address is fine, and so is the default.
    let b = PortBinding {
        host_ip: Some("127.0.0.1".parse().unwrap()),
        host_port: 0,
        guest_port: 80,
    };
    assert!(m.publish(b).unwrap().local_addr().ip().is_loopback());
    m.remove(true).unwrap();
}

#[test]
fn configured_port_bindings_are_published_at_start_and_closed_at_exit() {
    let env = Env::new();
    let engine = env.engine();
    let guest_port = guest_service();
    let host_port = free_port();
    let host = HostConfig {
        port_bindings: vec![PortBinding::loopback(host_port, guest_port)],
        ..Default::default()
    };
    let marker = env.root().join("stop");
    let m = engine
        .create(
            Some("cfg"),
            sh(&format!(
                "while [ ! -e {} ]; do sleep 0.05; done",
                marker.display()
            )),
            host,
        )
        .unwrap();
    m.start().unwrap();
    let addr = SocketAddr::from(([127, 0, 0, 1], host_port));
    assert_eq!(talk(addr, b"hi"), "got 2 bytes: hi");

    std::fs::write(&marker, "").unwrap();
    m.wait().unwrap();
    assert!(refuses(addr), "listener must close when the machine exits");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A host-side service with the same protocol as `guest_service`.
fn host_service() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            std::thread::spawn(move || {
                let mut conn = conn;
                let mut seen = Vec::new();
                conn.read_to_end(&mut seen).unwrap();
                write!(conn, "host got {} bytes", seen.len()).unwrap();
            });
        }
    });
    port
}

fn guest_loopback(port: u16) -> SocketAddr {
    SocketAddr::new(LocalProcessVmm::GUEST_LOOPBACK.parse().unwrap(), port)
}

#[test]
fn host_loopback_service_is_reachable_from_the_guest_at_the_same_port() {
    let env = Env::new();
    let m = running(&env);
    let port = host_service();
    assert!(
        refuses(guest_loopback(port)),
        "not reachable before exposing"
    );

    let handle = m.expose_host_port(port).unwrap();
    assert_eq!(talk(guest_loopback(port), b"abc"), "host got 3 bytes");

    drop(handle);
    assert!(refuses(guest_loopback(port)));
    m.remove(true).unwrap();
}

#[test]
fn configured_host_gateway_ports_are_set_up_at_start() {
    let env = Env::new();
    let engine = env.engine();
    let port = host_service();
    let host = HostConfig {
        host_gateway_ports: vec![port],
        ..Default::default()
    };
    let m = engine.create(Some("gw"), sh("sleep 60"), host).unwrap();
    m.start().unwrap();
    assert_eq!(talk(guest_loopback(port), b""), "host got 0 bytes");
    m.remove(true).unwrap();
}

#[test]
fn host_unix_socket_is_reachable_at_the_guest_path() {
    let env = Env::new();
    let m = running(&env);
    let host_path = env.root().join("host.sock");
    let guest_path = env.root().join("guest/var/run/clankerd.sock");
    let l = std::os::unix::net::UnixListener::bind(&host_path).unwrap();
    std::thread::spawn(move || {
        for conn in l.incoming().flatten() {
            let mut conn = conn;
            let mut seen = String::new();
            conn.read_to_string(&mut seen).unwrap();
            write!(conn, "daemon says hi to {seen}").unwrap();
        }
    });

    let handle = m.bind_socket(&guest_path, &host_path).unwrap();
    let mut c = std::os::unix::net::UnixStream::connect(&guest_path).unwrap();
    c.write_all(b"guest").unwrap();
    c.shutdown(Shutdown::Write).unwrap();
    let mut out = String::new();
    c.read_to_string(&mut out).unwrap();
    assert_eq!(out, "daemon says hi to guest");

    drop(handle);
    assert!(!guest_path.exists(), "guest socket removed on drop");
    m.remove(true).unwrap();
}

#[test]
fn the_guest_can_only_reach_targets_the_host_exposed() {
    use clankerd_proto::tunnel::{Connect, METHOD_HOST_CONNECT, Target};
    use clankerd_proto::varlink::{self, Call, Reply};

    let env = Env::new();
    let m = running(&env);
    let secret = host_service();
    let _allowed = m.expose_host_port(host_service()).unwrap();

    // What a compromised guest could do: dial the host endpoint directly.
    let host_socket = env
        .root()
        .join("run")
        .join(format!("{}.host", &m.id()[..12]));
    let mut conn = std::os::unix::net::UnixStream::connect(host_socket).unwrap();
    varlink::write(
        &mut conn,
        &Call {
            method: METHOD_HOST_CONNECT.into(),
            parameters: serde_json::to_value(Connect {
                target: Target::Tcp { port: secret },
            })
            .unwrap(),
            more: false,
            upgrade: true,
        },
    )
    .unwrap();
    let reply: Reply = varlink::read(&mut std::io::BufReader::new(conn))
        .unwrap()
        .unwrap();
    assert_eq!(reply.error.as_deref(), Some("io.clankerd.NotAllowed"));
    m.remove(true).unwrap();
}
