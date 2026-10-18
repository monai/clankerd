//! Observe public JSON through vmctl with a guest-protocol fixture.
#![cfg(target_os = "linux")]
#[path = "../../libclankerd/tests/common/mod.rs"]
mod domain;
use clankerd_proto::varlink::{self, Call, Reply};
use libclankerd::vmm::{BootHandle, BootSpec, LocalProcessVmm};
use libclankerd::{Engine, HostConfig, MachineConfig, Vmm};
use serde_json::{Value, json};
use std::io::{self, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

struct GuestEndpoint {
    vmm: LocalProcessVmm,
    socket: Mutex<Option<PathBuf>>,
}
impl Vmm for GuestEndpoint {
    fn boot(&self, spec: &BootSpec) -> libclankerd::Result<BootHandle> {
        *self.socket.lock().unwrap() = Some(spec.guest_socket.clone());
        self.vmm.boot(spec)
    }
}

#[test]
fn inspection_converts_guest_probe_seconds_to_utc_milliseconds() {
    let env = domain::Env::new();
    let guestd = domain::guestd_path();
    let helper = env.root().join("clankerd-network");
    std::fs::write(
        &helper,
        r#"#!/bin/sh
while [ "$#" -gt 0 ]; do
  case "$1" in --socket) sock=$2 ;; --nat66-prefix) prefix=$2 ;; esac
  shift
done
printf '{"pid":%s,"vmnet_mac_address":"0a:d6:36:c1:ea:f3","vmnet_mtu":1500,"vmnet_max_packet_size":1514,"vmnet_nat66_prefix":"%s"}\n' "$$" "$prefix"
: > "$sock"
trap 'exit 0' TERM
while :; do sleep 0.05; done
"#,
    )
    .unwrap();
    std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let endpoint = Arc::new(GuestEndpoint {
        vmm: LocalProcessVmm::new(&guestd),
        socket: Mutex::default(),
    });
    let mut config = env.config(endpoint.clone());
    config.net = Some(Arc::new(libclankerd::net::VmnetBackend::new(&helper)));
    let engine = Engine::new(config).unwrap();
    let machine = engine
        .create(
            Some("example"),
            MachineConfig {
                image: "img".into(),
                cmd: vec!["sleep".into(), "60".into()],
                ..Default::default()
            },
            HostConfig::default(),
        )
        .unwrap();
    machine.start().unwrap();
    let socket = endpoint.socket.lock().unwrap().clone().unwrap();
    let real_socket = socket.with_extension("real");
    std::fs::rename(&socket, &real_socket).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let seconds = Arc::new(AtomicU64::new(0));
    let stopped = Arc::new(AtomicBool::new(false));
    let proxy = std::thread::spawn({
        let seconds = seconds.clone();
        let stopped = stopped.clone();
        move || {
            while !stopped.load(Ordering::Relaxed) {
                let mut connection = match listener.accept() {
                    Ok((connection, _)) => connection,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(5));
                        continue;
                    }
                    Err(e) => panic!("{e}"),
                };
                let real_socket = real_socket.clone();
                let seconds = seconds.clone();
                std::thread::spawn(move || {
                    let Some(call) = varlink::read::<Call, _>(&mut BufReader::new(
                        connection.try_clone().unwrap(),
                    ))
                    .unwrap() else {
                        return;
                    };
                    if call.method == clankerd_proto::guest::METHOD_NETWORK_STATUS {
                        let probe = if seconds.load(Ordering::Relaxed) == u64::MAX {
                            Value::Null
                        } else {
                            json!({"destination":"127.0.0.1:443","connected":true,"checked_at_unix_seconds":seconds.load(Ordering::Relaxed),"diagnostic":"example"})
                        };
                        let family = json!({"local_available":true,"default_route":true,"external":"available","probe":probe});
                        let status = json!({"addresses":[],"address_details":[],"routes":[],"nameservers":[],"search":[],"ipv4":family,"ipv6":family,"ready":true,"diagnostic":"example"});
                        let _ = varlink::write(
                            &mut connection,
                            &Reply {
                                parameters: status,
                                continues: false,
                                error: None,
                            },
                        );
                    } else {
                        let mut guest = UnixStream::connect(real_socket).unwrap();
                        varlink::write(&mut guest, &call).unwrap();
                        let _ = io::copy(&mut guest, &mut connection);
                    }
                });
            }
        }
    });
    for (source, expected) in [
        (0, json!(0)),
        (1, json!(1000)),
        (1_700_000_000, json!(1_700_000_000_000u64)),
        (u64::MAX, Value::Null),
    ] {
        seconds.store(source, Ordering::Relaxed);
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_vmctl"))
            .args([
                "--vmspawn",
                "/unused-fixture-helper",
                "--network-executable",
                helper.to_str().unwrap(),
                "machine",
                "inspect",
                "example",
            ])
            .env_remove("CLANKERD_DEV_GUESTD")
            .env_remove("CLANKERD_VMSPAWN_DEV_LOCAL")
            .env_remove("CLANKERD_BOOT_DIR_ROOT")
            .env("CLANKERD_STATE_DIR", env.root().join("state"))
            .env("CLANKERD_RUNTIME_DIR", env.root().join("run"))
            .env("CLANKERD_CACHE_DIR", env.root().join("cache"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        let records: Value = serde_json::from_slice(&out.stdout).unwrap();
        for family in ["ipv4", "ipv6"] {
            let probe = &records[0]["network"][family]["probe"];
            if expected.is_null() {
                assert!(probe.is_null());
            } else {
                assert_eq!(probe["checked_at"], expected);
                assert!(probe.get("checked_at_unix_seconds").is_none());
                assert_eq!(probe["connected"], true);
            }
        }
    }
    machine.remove(true).unwrap();
    stopped.store(true, Ordering::Relaxed);
    proxy.join().unwrap();
}
