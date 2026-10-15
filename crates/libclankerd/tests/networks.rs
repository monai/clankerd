mod common;
use common::*;
use libclankerd::{Engine, ErrorKind, NetworkKind};

#[test]
fn virtual_network_lifecycle_rejects_overlapping_subnets_and_persists() {
    let env = Env::new();
    let engine = Engine::new(env.config(std::sync::Arc::new(
        libclankerd::vmm::LocalProcessVmm::new(guestd_path()),
    )))
    .unwrap();
    let network = engine
        .create_network(
            "team",
            NetworkKind::Shared,
            Some("192.168.120.0/24".parse().unwrap()),
        )
        .unwrap();
    assert_eq!(engine.inspect_network("team").unwrap(), network);
    assert_eq!(engine.networks().unwrap(), vec![network]);
    assert_eq!(
        engine
            .create_network(
                "overlap",
                NetworkKind::HostOnly,
                Some("192.168.120.0/25".parse().unwrap())
            )
            .unwrap_err()
            .kind(),
        ErrorKind::Conflict
    );
    engine.remove_network("team").unwrap();
    assert_eq!(
        engine.inspect_network("team").unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn membership_prevents_removal_and_can_change_only_while_stopped() {
    let env = Env::new();
    let engine = Engine::new(env.config(std::sync::Arc::new(
        libclankerd::vmm::LocalProcessVmm::new(guestd_path()),
    )))
    .unwrap();
    let network = engine
        .create_network("team", NetworkKind::HostOnly, None)
        .unwrap();
    let host = libclankerd::HostConfig {
        virtual_network: Some("team".into()),
        ..Default::default()
    };
    let machine = engine
        .create(Some("member"), sh("while true; do sleep 0.05; done"), host)
        .unwrap();
    let before = machine.inspect().unwrap();
    assert_eq!(
        before.host_config.network_mode,
        libclankerd::NetworkMode::HostOnly
    );
    assert_eq!(before.host_config.shared_network, network.subnet);
    assert_eq!(
        engine.remove_network("team").unwrap_err().kind(),
        ErrorKind::Conflict
    );
    let next = engine
        .create_network("next", NetworkKind::Shared, None)
        .unwrap();
    let host = libclankerd::HostConfig {
        virtual_network: Some("next".into()),
        ..Default::default()
    };
    machine.start().unwrap();
    assert_eq!(
        machine
            .update(&before.config, host.clone())
            .unwrap_err()
            .kind(),
        ErrorKind::Conflict
    );
    machine.stop(std::time::Duration::from_secs(10)).unwrap();
    machine.update(&before.config, host).unwrap();
    assert_eq!(
        machine.inspect().unwrap().host_config.shared_network,
        next.subnet
    );
    engine.remove_network("team").unwrap();
    machine.remove(false).unwrap();
    engine.remove_network("next").unwrap();
}

#[test]
fn concurrent_engines_allocate_distinct_subnets_and_duplicate_names_conflict() {
    let env = Env::new();
    let state = env.root().join("state");
    let runtime = env.root().join("runtime");
    let mut workers = Vec::new();
    for i in 0..8 {
        let state = state.clone();
        let runtime = runtime.clone();
        workers.push(std::thread::spawn(move || {
            let engine = Engine::new(libclankerd::EngineConfig::new(state, runtime)).unwrap();
            engine
                .create_network(&format!("network-{i}"), NetworkKind::Shared, None)
                .unwrap()
        }));
    }
    let allocated: Vec<_> = workers
        .into_iter()
        .map(|worker| worker.join().unwrap().subnet.address)
        .collect();
    assert_eq!(
        allocated
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        8
    );
    let engine = Engine::new(libclankerd::EngineConfig::new(state, runtime)).unwrap();
    assert_eq!(engine.networks().unwrap().len(), 8);
    assert_eq!(
        engine
            .create_network("network-0", NetworkKind::HostOnly, None)
            .unwrap_err()
            .kind(),
        ErrorKind::Conflict
    );
}

#[test]
fn legacy_peer_opt_out_allocates_a_singleton_network_instead_of_reusing_the_subnet() {
    let env = Env::new();
    let engine = Engine::new(env.config(std::sync::Arc::new(
        libclankerd::vmm::LocalProcessVmm::new(guestd_path()),
    )))
    .unwrap();
    let a = engine
        .create(Some("plain"), sh("true"), Default::default())
        .unwrap();
    let b = engine
        .create(
            Some("singleton"),
            sh("true"),
            libclankerd::HostConfig {
                shared_peer_participation: false,
                ..Default::default()
            },
        )
        .unwrap();
    let a = a.inspect().unwrap().host_config;
    let b = b.inspect().unwrap().host_config;
    assert_ne!(a.virtual_network, b.virtual_network);
    assert_ne!(a.shared_network, b.shared_network);
    assert!(
        b.shared_peer_participation,
        "no per-member veto in canonical model"
    );
}

#[test]
fn implicit_network_names_cannot_change_requested_mode_or_subnet() {
    let env = Env::new();
    let engine = Engine::new(env.config(std::sync::Arc::new(
        libclankerd::vmm::LocalProcessVmm::new(guestd_path()),
    )))
    .unwrap();
    engine
        .create_network(
            "shared-192.168.105.0-24",
            NetworkKind::HostOnly,
            Some("192.168.120.0/24".parse().unwrap()),
        )
        .unwrap();
    assert_eq!(
        engine
            .create(Some("plain"), sh("true"), Default::default())
            .err()
            .unwrap()
            .kind(),
        ErrorKind::Conflict
    );
}

#[test]
fn repeated_legacy_singleton_updates_preserve_membership() {
    let env = Env::new();
    let engine = Engine::new(env.config(std::sync::Arc::new(
        libclankerd::vmm::LocalProcessVmm::new(guestd_path()),
    )))
    .unwrap();
    let machine = engine
        .create(Some("member"), sh("true"), Default::default())
        .unwrap();
    let host = libclankerd::HostConfig {
        shared_peer_participation: false,
        ..Default::default()
    };
    let config = machine.inspect().unwrap().config;
    machine.update(&config, host.clone()).unwrap();
    let first = machine.inspect().unwrap().host_config;
    machine.update(&config, host).unwrap();
    assert_eq!(machine.inspect().unwrap().host_config, first);
}

#[test]
fn missing_network_update_does_not_create_a_volume() {
    let env = Env::new();
    let engine = Engine::new(env.config(std::sync::Arc::new(
        libclankerd::vmm::LocalProcessVmm::new(guestd_path()),
    )))
    .unwrap();
    let machine = engine
        .create(Some("member"), sh("true"), Default::default())
        .unwrap();
    let config = machine.inspect().unwrap().config;
    let host = libclankerd::HostConfig {
        virtual_network: Some("missing".into()),
        mounts: vec![libclankerd::Mount::volume("must-not-exist", "/storage")],
        ..Default::default()
    };
    assert_eq!(
        machine.update(&config, host).unwrap_err().kind(),
        ErrorKind::NotFound
    );
    assert_eq!(
        engine.volume("must-not-exist").unwrap_err().kind(),
        ErrorKind::NotFound
    );
}

#[test]
fn shared_prefixes_persist_follow_membership_and_are_distinct() {
    let env = Env::new();
    let engine = Engine::new(env.config(std::sync::Arc::new(
        libclankerd::vmm::LocalProcessVmm::new(guestd_path()),
    )))
    .unwrap();
    let one = engine
        .create_network("one", NetworkKind::Shared, None)
        .unwrap();
    let two = engine
        .create_network("two", NetworkKind::Shared, None)
        .unwrap();
    assert!(one.ipv6_prefix.is_some());
    assert_ne!(one.ipv6_prefix, two.ipv6_prefix);
    for prefix in [one.ipv6_prefix.unwrap(), two.ipv6_prefix.unwrap()] {
        assert_eq!(prefix.octets()[0], 0xfd);
        assert_eq!(u128::from(prefix) & u128::from(u64::MAX), 0);
    }
    let machine = engine
        .create(
            Some("member"),
            sh("true"),
            libclankerd::HostConfig {
                virtual_network: Some("one".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        machine.inspect().unwrap().host_config.shared_ipv6_prefix,
        one.ipv6_prefix
    );
    machine
        .update(
            &machine.inspect().unwrap().config,
            libclankerd::HostConfig {
                virtual_network: Some("two".into()),
                ..Default::default()
            },
        )
        .unwrap();
    assert_eq!(
        machine.inspect().unwrap().host_config.shared_ipv6_prefix,
        two.ipv6_prefix
    );
    assert_eq!(
        engine.inspect_network("one").unwrap().ipv6_prefix,
        one.ipv6_prefix
    );
    assert!(
        engine
            .create_network("host", NetworkKind::HostOnly, None)
            .unwrap()
            .ipv6_prefix
            .is_none()
    );
}
