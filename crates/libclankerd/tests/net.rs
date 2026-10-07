//! Seam A: the network sidecar. The only fakes are the `Vmm` (guestd as a
//! local process) and the gvproxy binary (a shell script that makes its socket
//! and waits to be signalled, like the real one).

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clankerd_proto::guest::NetworkConfig;
use common::*;
use libclankerd::net::{GvproxyBackend, NetAttachment};
use libclankerd::vmm::{BootHandle, BootSpec, LocalProcessVmm};
use libclankerd::{Engine, ErrorKind, HostConfig, Status, Vmm};

const FAKE_GVPROXY: &str = r#"#!/bin/sh
echo "$@" > "$0.args"
sock=""
while [ $# -gt 0 ]; do
  case "$1" in -listen-vfkit) sock=${2#unixgram://} ;; esac
  shift
done
echo $$ > "$0.pid"
: > "$sock"
trap 'rm -f "$sock"; exit 0' TERM
while :; do sleep 0.1; done
"#;

const FAILING_GVPROXY: &str = "#!/bin/sh\necho 'cannot listen: address in use' >&2\nexit 3\n";

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("gvproxy");
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
    gvproxy: PathBuf,
}

fn rig(gvproxy_body: &str) -> Rig {
    let env = Env::new();
    let gvproxy = script(env.root(), gvproxy_body);
    let vmm = Arc::new(Recording {
        inner: LocalProcessVmm::new(guestd_path()),
        specs: Mutex::default(),
    });
    let mut cfg = env.config(vmm.clone());
    cfg.net = Some(Arc::new(GvproxyBackend::new(&gvproxy)));
    let engine = Engine::new(cfg).unwrap();
    Rig {
        _env: env,
        engine,
        vmm,
        gvproxy,
    }
}

impl Rig {
    fn sidecar_pid(&self) -> i32 {
        let path = format!("{}.pid", self.gvproxy.display());
        std::fs::read_to_string(path)
            .unwrap()
            .trim()
            .parse()
            .unwrap()
    }
    fn args(&self) -> String {
        std::fs::read_to_string(format!("{}.args", self.gvproxy.display())).unwrap()
    }
    fn last_spec(&self) -> BootSpec {
        self.vmm.specs.lock().unwrap().last().unwrap().clone()
    }
    fn net(&self) -> NetAttachment {
        self.last_spec().net.unwrap()
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
    let rig = rig(FAKE_GVPROXY);
    let m = long_running(&rig.engine, "net-stop");
    m.start().unwrap();
    let pid = rig.sidecar_pid();
    assert!(alive(pid));
    m.stop(Duration::from_secs(10)).unwrap();
    wait_gone(pid);
}

#[test]
fn the_sidecar_ends_when_the_machine_is_killed_or_removed() {
    let rig = rig(FAKE_GVPROXY);
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
    let rig = rig(FAKE_GVPROXY);
    let m = create(&rig.engine, "net-exit", "sleep 0.3");
    m.start().unwrap();
    let pid = rig.sidecar_pid();
    m.wait().unwrap();
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
    wait_gone(pid);
}

#[test]
fn gvproxy_listens_for_the_vmm_and_exposes_no_host_control_api() {
    let rig = rig(FAKE_GVPROXY);
    let m = long_running(&rig.engine, "net-args");
    m.start().unwrap();
    let args = rig.args();
    let sock = rig.net().socket;
    assert!(
        args.contains(&format!("-listen-vfkit unixgram://{}", sock.display())),
        "{args}"
    );
    assert!(
        args.contains("-ssh-port -1"),
        "default ssh forward must be off: {args}"
    );
    for flag in ["-listen ", "-services", "-forward-"] {
        assert!(!args.contains(flag), "{flag} must not be passed: {args}");
    }
    m.kill(libc::SIGKILL).unwrap();
}

#[test]
fn the_vmm_gets_a_nic_and_the_guest_is_told_what_it_must_not_reach() {
    let rig = rig(FAKE_GVPROXY);
    let m = long_running(&rig.engine, "net-spec");
    m.start().unwrap();
    let spec = rig.last_spec();
    let net = spec.net.unwrap();
    assert!(net.vfkit);
    assert_eq!(net.mac, [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee]);
    assert!(
        net.socket.exists(),
        "the VMM connects to an existing socket"
    );

    let NetworkConfig {
        interface,
        hostname,
        blocked,
    } = spec.workload.network.unwrap();
    assert_eq!(interface, "eth0");
    assert_eq!(hostname, spec.machine_id[..12]);
    let gateway_api = blocked
        .iter()
        .find(|b| b.addr.to_string() == "192.168.127.1")
        .unwrap();
    assert_eq!(gateway_api.tcp_port, Some(80));
    let host_loopback = blocked
        .iter()
        .find(|b| b.addr.to_string() == "192.168.127.254")
        .unwrap();
    assert_eq!(host_loopback.tcp_port, None);
    m.kill(libc::SIGKILL).unwrap();
}

#[test]
fn a_sidecar_that_dies_at_startup_fails_the_start_with_its_message() {
    let rig = rig(FAILING_GVPROXY);
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
    let gvproxy = script(env.root(), FAKE_GVPROXY);
    let mut cfg = env.config(Arc::new(
        libclankerd::vmm::VmspawnVmm::new(vmspawn_path(), guestd_path()).dev_local(),
    ));
    cfg.net = Some(Arc::new(GvproxyBackend::new(&gvproxy)));
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
    assert_eq!(
        net["mac"],
        serde_json::json!([0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee])
    );
    assert_eq!(net["vfkit"], true);
    assert!(net["socket"].as_str().unwrap().ends_with(".net"), "{net}");
    assert_ne!(net["features"].as_u64().unwrap(), 0);
    m.kill(libc::SIGKILL).unwrap();
}
