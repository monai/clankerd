//! Network lifecycle through typed daemon requests; guestd runs locally.
mod common;
#[path = "common/network.rs"]
mod network;
use common::*;
use libclankerd::net::Network;
use libclankerd::vmm::{BootHandle, BootSpec, LocalProcessVmm};
use libclankerd::{Engine, ErrorKind, HostConfig, Status, Vmm};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
    service: Arc<network::Service>,
}
fn rig() -> Rig {
    rig_with_probes(None)
}
fn rig_with_probes(probe_endpoints: Option<serde_json::Value>) -> Rig {
    rig_with_service(Arc::default(), probe_endpoints)
}
fn rig_with_service(
    service: Arc<network::Service>,
    probe_endpoints: Option<serde_json::Value>,
) -> Rig {
    let env = Env::new();
    let vmm = Arc::new(Recording {
        inner: LocalProcessVmm::new(guestd_path()),
        specs: Mutex::default(),
        probe_endpoints,
    });
    let mut cfg = env.config(vmm.clone());
    cfg.net = Some(Arc::new(Network::with_service(service.clone())));
    let engine = Engine::new(cfg).unwrap();
    Rig {
        _env: env,
        engine,
        vmm,
        service,
    }
}
impl Rig {
    fn last_spec(&self) -> BootSpec {
        self.vmm.specs.lock().unwrap().last().unwrap().clone()
    }
}
fn long_running(engine: &Engine, name: &str) -> libclankerd::Machine {
    create(engine, name, "while true; do sleep 0.05; done")
}

#[test]
fn interfaces_follow_machine_lifetime_and_not_workload_lifetime() {
    let rig = rig();
    let a = create(&rig.engine, "a", "sleep 0.2");
    let b = long_running(&rig.engine, "b");
    a.start().unwrap();
    let first = rig.last_spec().net.unwrap();
    assert_eq!(first.generation, "1");
    b.start().unwrap();
    let second = rig.last_spec().net.unwrap();
    assert_eq!(second.generation, "2");
    assert_ne!(first.key, second.key);
    assert_eq!(rig.service.count(), 2);
    a.wait_workload().unwrap();
    assert_eq!(a.inspect().unwrap().state.status, Status::Running);
    assert_eq!(rig.service.count(), 2);
    a.stop(Duration::from_secs(10)).unwrap();
    assert_eq!(rig.service.count(), 1);
    b.kill(libc::SIGKILL).unwrap();
    assert_eq!(rig.service.count(), 0);
    b.start().unwrap();
    let restarted = rig.last_spec().net.unwrap();
    assert_eq!(restarted.key, second.key);
    assert_eq!(restarted.mac, second.mac);
    assert_eq!(restarted.generation, "3");
    assert_eq!(rig.service.count(), 1);
    b.remove(true).unwrap();
    assert_eq!(rig.service.count(), 0);
}
#[test]
fn daemon_failure_prevents_vm_boot_and_reports_its_error() {
    let rig = rig();
    *rig.service.failure.lock().unwrap() = Some("daemon unavailable".into());
    let m = long_running(&rig.engine, "fail");
    let error = m.start().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unavailable);
    assert!(error.message().contains("daemon unavailable"));
    assert!(rig.vmm.specs.lock().unwrap().is_empty());
    assert_eq!(m.inspect().unwrap().state.status, Status::Created);
}
#[test]
fn daemon_loss_does_not_recreate_interfaces_until_explicit_restart() {
    let rig = rig();
    let m = long_running(&rig.engine, "lost");
    m.start().unwrap();
    rig.service.entries.lock().unwrap().clear();
    let info = m.inspect().unwrap();
    assert_eq!(info.state.status, Status::Running);
    assert!(!info.network.unwrap().ready);
    assert_eq!(rig.service.history.lock().unwrap().len(), 1);
    m.stop(Duration::from_secs(10)).unwrap();
    m.start().unwrap();
    assert_eq!(rig.service.history.lock().unwrap().len(), 2);
    assert_eq!(rig.service.count(), 1);
    m.remove(true).unwrap();
}
#[test]
fn typed_settings_preserve_network_modes_addresses_and_prefixes() {
    for (name, mode, expected) in [
        ("shared", libclankerd::NetworkMode::Shared, "shared"),
        ("host", libclankerd::NetworkMode::HostOnly, "host"),
        ("bridge", libclankerd::NetworkMode::Bridged, "bridged"),
    ] {
        let rig = rig();
        let host = HostConfig {
            network_mode: mode,
            bridge_interface: (expected == "bridged").then(|| "en0".into()),
            ..Default::default()
        };
        let m = rig.engine.create(Some(name), sh("sleep 60"), host).unwrap();
        m.start().unwrap();
        let initial = rig.service.last();
        assert_eq!(initial.operation_mode, expected);
        if expected == "bridged" {
            assert_eq!(initial.shared_interface.as_deref(), Some("en0"));
            assert!(initial.start_address.is_none());
        } else {
            assert!(initial.start_address.is_some());
            assert!(initial.network_id.is_some());
        }
        assert_eq!(initial.nat66_prefix.is_some(), expected == "shared");
        m.stop(Duration::from_secs(10)).unwrap();
        m.start().unwrap();
        let again = rig.service.last();
        assert_eq!(initial.interface_id, again.interface_id);
        assert_eq!(initial.network_id, again.network_id);
        assert_eq!(initial.nat66_prefix, again.nat66_prefix);
        m.remove(true).unwrap();
    }
}
#[test]
fn wrong_prefix_rolls_back_interface_before_vm_boot() {
    let rig = rig_with_service(
        Arc::new(network::Service {
            wrong_prefix: true,
            ..Default::default()
        }),
        None,
    );
    let m = long_running(&rig.engine, "prefix");
    assert!(m.start().is_err());
    assert_eq!(rig.service.count(), 0);
    assert!(rig.vmm.specs.lock().unwrap().is_empty());
}
#[test]
fn membership_changes_are_sent_to_daemon_only_after_stopping() {
    let rig = rig();
    let a = rig
        .engine
        .create_network("first", libclankerd::NetworkKind::HostOnly, None)
        .unwrap();
    let b = rig
        .engine
        .create_network("second", libclankerd::NetworkKind::HostOnly, None)
        .unwrap();
    let m = rig
        .engine
        .create(
            Some("member"),
            sh("sleep 60"),
            HostConfig {
                virtual_network: Some(a.name),
                ..Default::default()
            },
        )
        .unwrap();
    m.start().unwrap();
    assert_eq!(rig.service.last().network_id, Some(a.id));
    let info = m.inspect().unwrap();
    let next = HostConfig {
        virtual_network: Some(b.name),
        ..Default::default()
    };
    assert_eq!(
        m.update(&info.config, next.clone()).unwrap_err().kind(),
        ErrorKind::Conflict
    );
    m.stop(Duration::from_secs(10)).unwrap();
    m.update(&info.config, next).unwrap();
    m.start().unwrap();
    assert_eq!(rig.service.last().network_id, Some(b.id));
    m.remove(true).unwrap();
}
#[test]
fn without_networking_machines_boot_without_a_nic() {
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
    common::finish_workload_and_stop(&m).unwrap();
    let spec = vmm.specs.lock().unwrap()[0].clone();
    assert!(spec.net.is_none());
    assert!(spec.workload.network.is_none());
}

#[test]
fn network_identity_is_unique_and_survives_restart() {
    let rig = rig();
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
fn inspection_reports_both_families_without_claiming_an_unconfigured_network_is_usable() {
    let rig = rig();
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
        let rig = rig_with_probes(Some(serde_json::json!({
            "ipv4":destination4.to_string(), "ipv6":destination6.to_string()
        })));
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
    let rig = rig();
    for config in [
        serde_json::json!({"network_mode":"bridged"}),
        serde_json::json!({"network_mode":"bridged","bridge_interface":""}),
        serde_json::json!({"network_mode":"bridged","bridge_interface":"../en0"}),
        serde_json::json!({"network_mode":"shared","bridge_interface":"en0"}),
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
fn an_unsupported_physical_interface_fails_before_the_workload_is_launched() {
    let rig = rig();
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
    assert_eq!(rig.service.count(), 0);
}

#[test]
fn invalid_shared_network_selection_fails_before_creating_or_attaching_a_machine() {
    let rig = rig();
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
fn host_only_members_share_the_same_virtual_network() {
    let rig = rig();
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
}
