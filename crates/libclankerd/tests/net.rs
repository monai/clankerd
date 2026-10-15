//! Seam A: the network sidecar. The only fakes are the `Vmm` (guestd as a
//! local process) and the vmnet-helper binary (a shell script that makes its socket
//! and waits to be signalled, like the real one).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use libclankerd::net::VmnetBackend;
use libclankerd::vmm::{BootHandle, BootSpec, LocalProcessVmm};
use libclankerd::{Engine, ErrorKind, HostConfig, Status, Vmm};

const FAKE_VMNET: &str = r#"#!/bin/sh
echo "$@" > "$0.args"
sock=""
prefix=""
while [ $# -gt 0 ]; do
  case "$1" in --socket) sock=$2 ;; --nat66-prefix) prefix=$2 ;; esac
  shift
done
echo $$ > "$0.pid"
echo 'INFO starting interface' >&2
if [ -n "$prefix" ]; then
  printf '{"vmnet_mac_address":"0a:d6:36:c1:ea:f3","vmnet_mtu":1500,"vmnet_max_packet_size":1514,"vmnet_nat66_prefix":"%s"}\n' "$prefix"
else
echo '{"vmnet_mac_address":"0a:d6:36:c1:ea:f3","vmnet_mtu":1500,"vmnet_max_packet_size":1514}'
fi
: > "$sock"
trap 'rm -f "$sock"; exit 0' TERM
while :; do sleep 0.1; done
"#;

const FAILING_VMNET: &str = "#!/bin/sh\necho 'cannot listen: address in use' >&2\nexit 3\n";

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("vmnet-helper");
    std::fs::write(&path, body).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

/// Remembers every BootSpec, then boots like the local stand-in.
struct Recording {
    inner: LocalProcessVmm,
    specs: Mutex<Vec<BootSpec>>,
    probe_endpoints: Option<serde_json::Value>,
}

impl Vmm for Recording {
    fn boot(&self, spec: &BootSpec) -> libclankerd::Result<BootHandle> {
        let mut spec = spec.clone();
        if let Some(endpoints) = &self.probe_endpoints {
            let mut config = serde_json::to_value(spec.workload.network.as_ref().unwrap()).unwrap();
            config["probe_endpoints"] = endpoints.clone();
            spec.workload.network = Some(serde_json::from_value(config).unwrap());
        }
        self.specs.lock().unwrap().push(spec.clone());
        self.inner.boot(&spec)
    }
}

struct Rig {
    _env: Env,
    engine: Engine,
    vmm: Arc<Recording>,
    helper: PathBuf,
}

fn rig(helper_body: &str) -> Rig {
    rig_with_probes(helper_body, None)
}
fn rig_with_probes(helper_body: &str, probe_endpoints: Option<serde_json::Value>) -> Rig {
    let env = Env::new();
    let helper = script(env.root(), helper_body);
    let vmm = Arc::new(Recording {
        inner: LocalProcessVmm::new(guestd_path()),
        specs: Mutex::default(),
        probe_endpoints,
    });
    let mut cfg = env.config(vmm.clone());
    cfg.net = Some(Arc::new(VmnetBackend::new(&helper)));
    let engine = Engine::new(cfg).unwrap();
    Rig {
        _env: env,
        engine,
        vmm,
        helper,
    }
}

impl Rig {
    fn sidecar_pid(&self) -> i32 {
        let path = format!("{}.pid", self.helper.display());
        std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    fn args(&self) -> String {
        std::fs::read_to_string(format!("{}.args", self.helper.display())).unwrap()
    }
    fn last_spec(&self) -> BootSpec {
        self.vmm.specs.lock().unwrap().last().unwrap().clone()
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_gone(pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while alive(pid) {
        assert!(Instant::now() < deadline, "sidecar {pid} is still running");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn long_running(engine: &Engine, name: &str) -> libclankerd::Machine {
    create(engine, name, "while true; do sleep 0.05; done")
}

#[test]
fn the_sidecar_runs_while_the_machine_runs_and_ends_with_a_stop() {
    let rig = rig(FAKE_VMNET);
    let m = long_running(&rig.engine, "net-stop");
    m.start().unwrap();
    let pid = rig.sidecar_pid();
    assert!(alive(pid));
    m.stop(Duration::from_secs(10)).unwrap();
    wait_gone(pid);
}

#[test]
fn the_sidecar_ends_when_the_machine_is_killed_or_removed() {
    let rig = rig(FAKE_VMNET);
    let m = long_running(&rig.engine, "net-kill");
    m.start().unwrap();
    let pid = rig.sidecar_pid();
    m.kill(libc::SIGKILL).unwrap();
    wait_gone(pid);

    m.start().unwrap();
    let again = rig.sidecar_pid();
    assert_ne!(again, pid);
    assert!(alive(again));
    m.remove(true).unwrap();
    wait_gone(again);
}

#[test]
fn the_sidecar_ends_when_the_workload_exits_by_itself() {
    let rig = rig(FAKE_VMNET);
    let m = create(&rig.engine, "net-exit", "sleep 0.3");
    m.start().unwrap();
    let pid = rig.sidecar_pid();
    m.wait().unwrap();
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
    wait_gone(pid);
}

#[test]
fn shared_network_attaches_without_offloads_or_endpoint_guards() {
    let rig = rig(FAKE_VMNET);
    let m = long_running(&rig.engine, "net-spec");
    m.start().unwrap();
    let spec = rig.last_spec();
    let net = spec.net.unwrap();
    assert!(!net.vfkit);
    assert_eq!(net.features, 0);
    assert!(rig.args().contains("--operation-mode shared"));
    assert!(rig.args().contains("--interface-id"));
    assert!(rig.args().contains("--start-address 192.168.105.1"));
    assert!(net.socket.exists());
    assert_eq!(spec.workload.network.unwrap().interface, "eth0");
    m.kill(libc::SIGKILL).unwrap();
}

#[test]
fn a_sidecar_that_dies_at_startup_fails_the_start_with_its_message() {
    let rig = rig(FAILING_VMNET);
    let m = long_running(&rig.engine, "net-fail");
    let err = m.start().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(
        err.message().contains("address in use"),
        "{}",
        err.message()
    );
    assert_eq!(m.inspect().unwrap().state.status, Status::Created);
    assert!(
        rig.vmm.specs.lock().unwrap().is_empty(),
        "no VMM without a network"
    );
}

#[test]
fn without_a_net_backend_machines_boot_without_a_nic() {
    let env = Env::new();
    let vmm = Arc::new(Recording {
        inner: LocalProcessVmm::new(guestd_path()),
        specs: Mutex::default(),
        probe_endpoints: None,
    });
    let engine = Engine::new(env.config(vmm.clone())).unwrap();
    let m = engine
        .create(Some("plain"), sh("sleep 0.1"), HostConfig::default())
        .unwrap();
    m.start().unwrap();
    m.wait().unwrap();
    let spec = vmm.specs.lock().unwrap()[0].clone();
    assert!(spec.net.is_none());
    assert!(spec.workload.network.is_none());
}

#[test]
fn the_vmspawn_helper_is_told_to_attach_the_nic() {
    let env = Env::new();
    let helper = script(env.root(), FAKE_VMNET);
    let mut cfg = env.config(Arc::new(
        libclankerd::vmm::VmspawnVmm::new(vmspawn_path(), guestd_path()).dev_local(),
    ));
    cfg.net = Some(Arc::new(VmnetBackend::new(&helper)));
    let engine = Engine::new(cfg).unwrap();
    let m = long_running(&engine, "net-helper");
    m.start().unwrap();

    let spec_file = env
        .root()
        .join("state/machines")
        .join(m.id())
        .join("vmspawn.json");
    let spec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(spec_file).unwrap()).unwrap();
    let net = &spec["net"];
    assert_eq!(net["vfkit"], false);
    assert_eq!(net["features"], 0);
    assert!(net["socket"].as_str().unwrap().ends_with(".net"));
    m.kill(libc::SIGKILL).unwrap();
}

#[test]
fn network_identity_is_unique_and_survives_restart() {
    let rig = rig(FAKE_VMNET);
    let first = long_running(&rig.engine, "identity-one");
    let second = long_running(&rig.engine, "identity-two");
    first.start().unwrap();
    let initial = rig.last_spec().net.unwrap().mac;
    let persisted = serde_json::to_value(first.inspect().unwrap()).unwrap();
    assert_eq!(persisted["host_config"]["network_mode"], "shared");
    first.stop(Duration::from_secs(10)).unwrap();
    first.start().unwrap();
    assert_eq!(rig.last_spec().net.unwrap().mac, initial);
    first.stop(Duration::from_secs(10)).unwrap();
    second.start().unwrap();
    assert_ne!(rig.last_spec().net.unwrap().mac, initial);
    second.stop(Duration::from_secs(10)).unwrap();
}

#[test]
fn a_helper_that_creates_a_socket_then_fails_does_not_launch_the_workload() {
    let rig = rig(r#"#!/bin/sh
while [ $# -gt 0 ]; do
  case "$1" in --socket) sock=$2 ;; esac
  shift
done
: > "$sock"
echo 'interface setup failed after binding socket' >&2
exit 3
"#);
    let machine = long_running(&rig.engine, "partial-attachment");
    let error = machine.start().unwrap_err();
    assert!(error.message().contains("interface setup failed"));
    assert!(rig.vmm.specs.lock().unwrap().is_empty());
    assert!(
        !rig._env
            .root()
            .join("state/machines")
            .join(machine.id())
            .join("net.pid")
            .exists()
    );
}

#[test]
fn inspection_reports_both_families_without_claiming_an_unconfigured_network_is_usable() {
    let rig = rig(FAKE_VMNET);
    let machine = long_running(&rig.engine, "family-status");
    machine.start().unwrap();
    let info = serde_json::to_value(machine.inspect().unwrap()).unwrap();
    assert_eq!(info["network"]["ipv4"]["local_available"], false);
    assert_eq!(info["network"]["ipv6"]["local_available"], false);
    assert_eq!(info["network"]["ipv4"]["external"], "unavailable");
    assert_eq!(info["network"]["ipv6"]["external"], "unavailable");
    assert_eq!(info["network"]["ready"], false);
    machine.stop(Duration::from_secs(10)).unwrap();
}

#[test]
fn external_observations_report_actual_family_specific_endpoint_success_and_failure() {
    for ipv4_connected in [true, false] {
        let ipv4 = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ipv6 = std::net::TcpListener::bind("[::1]:0").unwrap();
        let destination4 = ipv4.local_addr().unwrap();
        let destination6 = ipv6.local_addr().unwrap();
        let _listener = if ipv4_connected {
            drop(ipv6);
            ipv4
        } else {
            drop(ipv4);
            ipv6
        };
        let rig = rig_with_probes(
            FAKE_VMNET,
            Some(serde_json::json!({
                "ipv4":destination4.to_string(), "ipv6":destination6.to_string()
            })),
        );
        let machine = long_running(&rig.engine, "observed-endpoints");
        machine.start().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let info = serde_json::to_value(machine.inspect().unwrap()).unwrap();
            if !info["network"]["ipv4"]["probe"].is_null()
                && !info["network"]["ipv6"]["probe"].is_null()
            {
                for (family, destination, connected) in [
                    ("ipv4", destination4, ipv4_connected),
                    ("ipv6", destination6, !ipv4_connected),
                ] {
                    assert_eq!(
                        info["network"][family]["external"],
                        if connected {
                            "available"
                        } else {
                            "unavailable"
                        }
                    );
                    assert_eq!(info["network"][family]["probe"]["connected"], connected);
                    assert_eq!(
                        info["network"][family]["probe"]["destination"],
                        destination.to_string()
                    );
                    let diagnostic = info["network"][family]["probe"]["diagnostic"]
                        .as_str()
                        .unwrap();
                    assert!(diagnostic.contains(&destination.to_string()));
                    assert!(diagnostic.contains("other destinations are unverified"));
                    assert!(
                        info["network"][family]["probe"]["checked_at_unix_seconds"]
                            .as_u64()
                            .unwrap()
                            > 0
                    );
                }
                break;
            }
            assert!(
                Instant::now() < deadline,
                "no observed probe result: {info}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        machine.stop(Duration::from_secs(10)).unwrap();
    }
}

#[test]
fn bridged_network_requires_an_explicit_physical_interface() {
    let rig = rig(FAKE_VMNET);
    for config in [
        serde_json::json!({"network_mode":"bridged"}),
        serde_json::json!({"network_mode":"bridged","bridge_interface":""}),
        serde_json::json!({"network_mode":"bridged","bridge_interface":"../en0"}),
        serde_json::json!({"network_mode":"shared","bridge_interface":"en0"}),
        serde_json::json!({"network_mode":"bridged","bridge_interface":"en0","shared_peer_participation":false}),
        serde_json::json!({"network_mode":"bridged","bridge_interface":"en0","shared_network":{"address":"192.168.109.0","prefix_len":24}}),
    ] {
        let host: HostConfig = serde_json::from_value(config).unwrap();
        let error = rig
            .engine
            .create(Some("bridge-invalid"), sh("exit 0"), host)
            .err()
            .expect("invalid interface must fail");
        assert_eq!(error.kind(), ErrorKind::InvalidParameter);
        assert!(error.message().contains("interface"), "{error}");
        assert!(rig.vmm.specs.lock().unwrap().is_empty());
    }
}

#[test]
fn singleton_membership_and_selected_network_are_preserved_until_the_next_restart() {
    let rig = rig(FAKE_VMNET);
    let host: HostConfig = serde_json::from_value(serde_json::json!({
        "shared_network":{"address":"192.168.109.0","prefix_len":24},
        "shared_peer_participation":false
    }))
    .unwrap();
    let machine = rig
        .engine
        .create(
            Some("selected-network"),
            sh("while true; do sleep 0.05; done"),
            host,
        )
        .unwrap();
    machine.start().unwrap();
    let args = rig.args();
    let before = machine.inspect().unwrap();
    assert!(before.host_config.virtual_network.is_some());
    assert_ne!(
        before.host_config.shared_network.address.to_string(),
        "192.168.109.0"
    );
    let base = u32::from(before.host_config.shared_network.address);
    assert!(
        args.contains(&format!(
            "--start-address {}",
            std::net::Ipv4Addr::from(base + 1)
        )),
        "{args}"
    );
    assert!(
        !args.contains("--enable-isolation"),
        "membership replaces per-member isolation"
    );
    let next: HostConfig = serde_json::from_value(serde_json::json!({
        "shared_network":{"address":"192.168.110.0","prefix_len":25},
        "shared_peer_participation":true
    }))
    .unwrap();
    assert_eq!(
        machine
            .update(&before.config, next.clone())
            .unwrap_err()
            .kind(),
        ErrorKind::Conflict
    );
    assert_eq!(rig.args(), args, "a running attachment cannot be replaced");
    machine.stop(Duration::from_secs(10)).unwrap();
    machine.update(&before.config, next).unwrap();
    machine.start().unwrap();
    let changed = rig.args();
    assert!(
        changed.contains("--start-address 192.168.110.1"),
        "{changed}"
    );
    assert!(
        changed.contains("--end-address 192.168.110.126"),
        "{changed}"
    );
    assert!(
        changed.contains("--subnet-mask 255.255.255.128"),
        "{changed}"
    );
    assert!(!changed.contains("--enable-isolation"), "{changed}");
    machine.stop(Duration::from_secs(10)).unwrap();
    machine.start().unwrap();
    assert_eq!(
        rig.args(),
        changed,
        "selected network must persist across another restart"
    );
    machine.stop(Duration::from_secs(10)).unwrap();
}

const BRIDGE_VMNET: &str = r#"#!/bin/sh
if [ "$1" = --list-shared-interfaces ]; then echo en0; echo en1; exit 0; fi
echo "$@" > "$0.args"
while [ $# -gt 0 ]; do
  case "$1" in --socket) sock=$2 ;; esac
  shift
done
echo 'INFO starting interface' >&2
echo '{"vmnet_mac_address":"0a:d6:36:c1:ea:f3","vmnet_mtu":1500,"vmnet_max_packet_size":1514}'
: > "$sock"
trap 'rm -f "$sock"; exit 0' TERM
while :; do sleep 0.1; done
"#;

#[test]
fn a_selected_physical_interface_is_attached_and_preserved_across_restart() {
    let rig = rig(BRIDGE_VMNET);
    let host: HostConfig = serde_json::from_value(serde_json::json!({
        "network_mode":"bridged", "bridge_interface":"en0"
    }))
    .unwrap();
    let machine = rig
        .engine
        .create(
            Some("bridge-selected"),
            sh("while true; do sleep 0.05; done"),
            host,
        )
        .unwrap();
    machine.start().unwrap();
    assert!(
        rig.args().contains("--operation-mode bridged"),
        "{}",
        rig.args()
    );
    assert!(
        rig.args().contains("--shared-interface en0"),
        "{}",
        rig.args()
    );
    assert!(!rig.args().contains("--start-address"));
    assert!(!rig.args().contains("--enable-isolation"));
    let info = machine.inspect().unwrap();
    let mut next = info.host_config.clone();
    next.bridge_interface = Some("en1".into());
    assert_eq!(
        machine
            .update(&info.config, next.clone())
            .unwrap_err()
            .kind(),
        ErrorKind::Conflict
    );
    machine.stop(Duration::from_secs(10)).unwrap();
    machine.update(&info.config, next).unwrap();
    machine.start().unwrap();
    assert!(rig.args().contains("--shared-interface en1"));
    machine.stop(Duration::from_secs(10)).unwrap();
    machine.start().unwrap();
    assert!(rig.args().contains("--shared-interface en1"));
    machine.stop(Duration::from_secs(10)).unwrap();
}

#[test]
fn an_unsupported_physical_interface_fails_before_the_workload_is_launched() {
    let rig = rig(BRIDGE_VMNET);
    let host: HostConfig = serde_json::from_value(serde_json::json!({
        "network_mode":"bridged", "bridge_interface":"en9"
    }))
    .unwrap();
    let machine = rig
        .engine
        .create(Some("bridge-unavailable"), sh("exit 0"), host)
        .unwrap();
    let error = machine.start().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unavailable);
    assert!(error.message().contains("en9"));
    assert!(error.message().contains("unsupported"));
    assert!(rig.vmm.specs.lock().unwrap().is_empty());
    assert_eq!(machine.inspect().unwrap().state.status, Status::Created);
    assert!(
        !rig._env
            .root()
            .join("state/machines")
            .join(machine.id())
            .join("net.pid")
            .exists()
    );
}

#[test]
fn invalid_shared_network_selection_fails_before_creating_or_attaching_a_machine() {
    let rig = rig(FAKE_VMNET);
    for (address, prefix_len) in [
        ("192.168.109.3", 24),
        ("203.0.113.0", 24),
        ("10.0.0.0", 7),
        ("192.168.105.0", 31),
    ] {
        let host = serde_json::from_value(
            serde_json::json!({"shared_network":{"address":address,"prefix_len":prefix_len}}),
        )
        .unwrap();
        let error = rig
            .engine
            .create(Some("invalid-selection"), sh("exit 0"), host)
            .err()
            .expect("invalid network was accepted");
        assert_eq!(error.kind(), ErrorKind::InvalidParameter);
        assert_eq!(
            rig.engine.get("invalid-selection").err().unwrap().kind(),
            ErrorKind::NotFound
        );
        assert!(rig.vmm.specs.lock().unwrap().is_empty());
    }
}

#[test]
fn a_bound_socket_without_native_interface_metadata_is_not_a_ready_attachment() {
    let rig = rig(r#"#!/bin/sh
while [ $# -gt 0 ]; do
  case "$1" in --socket) sock=$2 ;; esac
  shift
done
: > "$sock"
echo 'interface still initializing, no completion metadata' >&2
trap 'rm -f "$sock"; exit 0' TERM
while :; do sleep 0.1; done
"#);
    let machine = long_running(&rig.engine, "socket-without-interface");
    let error = machine.start().unwrap_err();
    assert!(error.message().contains("no completion metadata"));
    assert!(rig.vmm.specs.lock().unwrap().is_empty());
    assert_eq!(machine.inspect().unwrap().state.status, Status::Created);
    assert!(
        !rig._env
            .root()
            .join("state/machines")
            .join(machine.id())
            .join("net.pid")
            .exists()
    );
}

#[test]
fn host_only_native_attachment_and_singleton_membership_persist() {
    let rig = rig(FAKE_VMNET);
    let host: HostConfig = serde_json::from_value(serde_json::json!({
        "network_mode":"host-only",
        "shared_network":{"address":"192.168.128.0","prefix_len":24},
        "shared_peer_participation":false
    }))
    .unwrap();
    let machine = rig
        .engine
        .create(
            Some("host-only"),
            sh("while true; do sleep 0.05; done"),
            host.clone(),
        )
        .unwrap();
    let assigned = machine.inspect().unwrap().host_config;
    for _ in 0..2 {
        machine.start().unwrap();
        let args = rig.args();
        assert!(args.contains("--operation-mode host"), "{args}");
        assert!(
            !args.contains("--enable-isolation"),
            "network members must retain peer communication: {args}"
        );
        assert!(!args.contains("--operation-mode shared"), "{args}");
        assert!(assigned.virtual_network.is_some());
        assert_eq!(machine.inspect().unwrap().host_config, assigned);
        machine.stop(Duration::from_secs(10)).unwrap();
    }
}

#[test]
fn host_only_members_share_the_same_virtual_network() {
    let rig = rig(FAKE_VMNET);
    let host = HostConfig {
        network_mode: libclankerd::NetworkMode::HostOnly,
        ..Default::default()
    };
    let a = rig
        .engine
        .create(Some("host-a"), sh("true"), host.clone())
        .unwrap();
    let b = rig.engine.create(Some("host-b"), sh("true"), host).unwrap();
    let a = a.inspect().unwrap().host_config;
    let b = b.inspect().unwrap().host_config;
    assert_eq!(a.virtual_network, b.virtual_network);
    assert!(a.shared_peer_participation && b.shared_peer_participation);
}

#[test]
fn named_shared_prefix_must_be_acknowledged_before_workload_start() {
    let rig = rig(FAKE_VMNET);
    let network = rig
        .engine
        .create_network("prefixed", libclankerd::NetworkKind::Shared, None)
        .unwrap();
    let machine = rig
        .engine
        .create(
            Some("prefixed-member"),
            sh("sleep 60"),
            HostConfig {
                virtual_network: Some(network.name),
                ..Default::default()
            },
        )
        .unwrap();
    machine.start().unwrap();
    let prefix = network.ipv6_prefix.unwrap();
    assert!(rig.args().contains(&format!("--nat66-prefix {prefix}")));
    assert_eq!(
        machine.inspect().unwrap().host_config.shared_ipv6_prefix,
        Some(prefix)
    );
    machine.remove(true).unwrap();
}

#[test]
fn wrong_native_prefix_prevents_workload_start() {
    let helper = FAKE_VMNET.replace("prefix=$2", "prefix=fd42:bad::");
    let rig = rig(&helper);
    rig.engine
        .create_network("prefixed", libclankerd::NetworkKind::Shared, None)
        .unwrap();
    let machine = rig
        .engine
        .create(
            Some("wrong-prefix"),
            sh("sleep 60"),
            HostConfig {
                virtual_network: Some("prefixed".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert!(machine.start().is_err());
    assert!(
        rig.vmm.specs.lock().unwrap().is_empty(),
        "VMM must not boot after an ignored prefix request"
    );
    machine.remove(true).unwrap();
}
