//! Unix socket bindings through the public Machine API.

mod common;

use std::io::{Read, Write};
use std::net::Shutdown;

use common::*;
use libclankerd::Machine;

fn running(env: &Env) -> Machine {
    let engine = env.engine();
    let m = create(&engine, "t", "sleep 60");
    m.start().unwrap();
    m
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
    let secret = env.root().join("secret.sock");

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
                target: Target::Unix { path: secret },
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
