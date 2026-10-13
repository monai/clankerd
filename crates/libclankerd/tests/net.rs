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
while [ $# -gt 0 ]; do
  case "$1" in --socket) sock=$2 ;; esac
  shift
done
echo $$ > "$0.pid"
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
fn selected_shared_network_and_peer_isolation_are_used_until_the_next_restart() {
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
    assert!(args.contains("--start-address 192.168.109.1"), "{args}");
    assert!(args.contains("--end-address 192.168.109.254"), "{args}");
    assert!(args.contains("--subnet-mask 255.255.255.0"), "{args}");
    assert!(args.contains("--enable-isolation"), "{args}");
    let before = machine.inspect().unwrap();
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
