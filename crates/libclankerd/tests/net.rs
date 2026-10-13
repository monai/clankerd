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
}

impl Vmm for Recording {
    fn boot(&self, spec: &BootSpec) -> libclankerd::Result<BootHandle> {
        self.specs.lock().unwrap().push(spec.clone());
        self.inner.boot(spec)
    }
}

struct Rig {
    _env: Env,
    engine: Engine,
    vmm: Arc<Recording>,
    helper: PathBuf,
}

fn rig(helper_body: &str) -> Rig {
    let env = Env::new();
    let helper = script(env.root(), helper_body);
    let vmm = Arc::new(Recording {
        inner: LocalProcessVmm::new(guestd_path()),
        specs: Mutex::default(),
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
