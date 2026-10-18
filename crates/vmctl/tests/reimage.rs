//! Reimage at the CLI boundary, with the existing tar-disk population fixture.
#[path = "../../libclankerd/tests/common/mod.rs"]
mod domain;
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
use std::sync::Arc;

#[test]
fn reimage_rebuilds_the_root_and_retains_volume_and_explicit_configuration() {
    let env = domain::Env::new();
    let registry = domain::registry::Registry::start();
    let guestd = domain::guestd_path();
    let cache = env.root().join("cache");
    let mut config = env.config(Arc::new(libclankerd::vmm::LocalProcessVmm::new(&guestd)));
    config.cache_dir = Some(cache.clone());
    config.insecure_registries = vec![registry.addr.clone()];
    config.populator = Some(Arc::new(domain::TarCapture::default()));
    let engine = libclankerd::Engine::new(config).unwrap();
    let mut refs = Vec::new();
    let mut digests = Vec::new();
    for (tag, command) in [("v1", "one"), ("v2", "two")] {
        digests.push(registry.push_with_config(
            "test/app",
            tag,
            &[domain::registry::layer(&[(tag, command.as_bytes())])],
            serde_json::json!({"Cmd":["echo",command],"Env":["IMAGE=default"]}),
        ));
        let reference = format!("{}/test/app:{tag}", registry.addr);
        // Prepare cached image bases through the existing population stand-in.
        engine
            .create(
                None,
                libclankerd::MachineConfig {
                    image: reference.clone(),
                    ..Default::default()
                },
                libclankerd::HostConfig::default(),
            )
            .unwrap()
            .remove(false)
            .unwrap();
        refs.push(reference);
    }
    let v = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_vmctl"))
            .args(args)
            .env("CLANKERD_STATE_DIR", env.root().join("state"))
            .env("CLANKERD_RUNTIME_DIR", env.root().join("run"))
            .env("CLANKERD_DEV_GUESTD", &guestd)
            .env("CLANKERD_GUEST_ROOT", domain::guest_root())
            .env("CLANKERD_CACHE_DIR", &cache)
            .env("CLANKERD_INSECURE_REGISTRIES", &registry.addr)
            .output()
            .unwrap()
    };
    let created = v(&[
        "machine",
        "create",
        "--name",
        "saved",
        "--cpus",
        "2",
        "-e",
        "MINE=override",
        "-v",
        "data:/storage",
        &refs[0],
    ]);
    assert!(created.status.success(), "{}", text(&created.stderr));
    let id = text(&created.stdout).trim().to_owned();
    let volume = v(&["volume", "inspect", "data"]);
    let volume: serde_json::Value = serde_json::from_slice(&volume.stdout).unwrap();
    let volume_path = volume[0]["path"].as_str().unwrap();
    use std::io::Write;
    std::fs::OpenOptions::new()
        .write(true)
        .open(volume_path)
        .unwrap()
        .write_all(b"precious")
        .unwrap();
    let alias_changed = v(&["reimage", "saved", &refs[1]]);
    assert!(
        alias_changed.status.success(),
        "{}",
        text(&alias_changed.stderr)
    );
    let alias_inspect = v(&["inspect", "saved"]);
    let alias_info: serde_json::Value = serde_json::from_slice(&alias_inspect.stdout).unwrap();
    assert_eq!(alias_info[0]["image_id"], digests[1]);
    assert!(
        v(&["machine", "reimage", "saved", &refs[0]])
            .status
            .success()
    );
    let changed = v(&["machine", "reimage", "saved", &refs[1]]);
    assert!(changed.status.success(), "{}", text(&changed.stderr));
    let inspected = v(&["inspect", "saved"]);
    let info: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(info[0]["image_id"], digests[1]);
    assert_eq!(info[0]["config"]["cmd"], serde_json::json!(["echo", "two"]));
    assert!(
        info[0]["config"]["env"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("MINE=override"))
    );
    assert_eq!(info[0]["host_config"]["cpus"], 2);
    assert_eq!(info[0]["host_config"]["mounts"][0]["name"], "data");
    let mut root = tar::Archive::new(
        std::fs::File::open(
            env.root()
                .join("state/machines")
                .join(id)
                .join(libclankerd::ROOT_DISK),
        )
        .unwrap(),
    );
    let paths: Vec<_> = root
        .entries()
        .unwrap()
        .map(|e| e.unwrap().path().unwrap().into_owned())
        .collect();
    assert!(paths.iter().any(|p| p == std::path::Path::new("v2")));
    assert!(!paths.iter().any(|p| p == std::path::Path::new("v1")));
    use std::io::Read;
    let mut marker = [0; 8];
    std::fs::File::open(volume_path)
        .unwrap()
        .read_exact(&mut marker)
        .unwrap();
    assert_eq!(&marker, b"precious");
    assert!(v(&["start", "saved"]).status.success());
    let rejected = v(&["reimage", "saved", &refs[0]]);
    assert_eq!(rejected.status.code(), Some(1));
    assert!(text(&rejected.stderr).contains("stop it first"));
    assert!(v(&["rm", "-f", "saved"]).status.success());
    assert!(v(&["volume", "inspect", "data"]).status.success());
}
