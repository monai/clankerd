mod common;
use common::{guestd, text, vmctl};

#[test]
fn canonical_machine_hierarchy_and_shortcuts_share_required_operands() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    for action in [
        "create", "start", "stop", "kill", "inspect", "rm", "exec", "logs", "reimage",
    ] {
        for args in [vec!["machine", action], vec![action]] {
            let out = vmctl(dir.path(), &guestd, &args);
            assert_eq!(
                out.status.code(),
                Some(2),
                "{args:?}: {}",
                text(&out.stderr)
            );
        }
    }
    let out = vmctl(dir.path(), &guestd, &["machine", "ls"]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert!(text(&out.stdout).contains("MACHINE ID"));
}

#[test]
fn explicit_json_and_partial_inspection_keep_stdout_parseable() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    let empty = v(&["network", "ls", "--output", "json"]);
    assert!(empty.status.success(), "{}", text(&empty.stderr));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&empty.stdout).unwrap(),
        serde_json::json!([])
    );
    assert!(v(&["network", "create", "team"]).status.success());
    let partial = v(&["network", "inspect", "--output", "json", "missing", "team"]);
    assert_eq!(partial.status.code(), Some(1));
    let records: serde_json::Value = serde_json::from_slice(&partial.stdout).unwrap();
    assert_eq!(records[0]["name"], "team");
    assert!(records[0]["created"].is_u64());
    let error: serde_json::Value = serde_json::from_slice(&partial.stderr).unwrap();
    assert_eq!(error["target"], "missing");
    assert_eq!(error["origin"], "operation");
    let out = v(&["network", "rm", "--output", "json", "missing", "team"]);
    let results: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(results[0]["success"], false);
    assert_eq!(results[1]["success"], true);
}

#[test]
fn grammar_enforces_option_scopes_and_preserves_guest_tail() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    for args in [
        vec!["machine", "ls", "--state-dir", "/tmp/invalid"],
        vec!["network", "create", "team", "--mode", "shared"],
        vec!["create", "--name", "one", "--name", "two", "img"],
        vec![
            "create",
            "--network",
            "team",
            "--bridge-interface",
            "en0",
            "img",
        ],
        vec!["kill", "--signal", "TERM", "missing"],
        vec!["rm", "-v", "missing"],
        vec!["run", "--output", "json", "img", "true"],
        vec!["exec", "--output", "json", "missing", "true"],
    ] {
        let out = v(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            text(&out.stderr)
        );
    }
    let out = v(&[
        "create", "--name", "literal", "img", "--memory", "2G", "--", "tail",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let info: serde_json::Value =
        serde_json::from_slice(&v(&["inspect", "literal"]).stdout).unwrap();
    assert_eq!(
        info[0]["config"]["cmd"],
        serde_json::json!(["--memory", "2G", "--", "tail"])
    );
    assert!(info[0]["host_config"]["memory"].is_null());
    let out = v(&[
        "create",
        "--name",
        "dash-image",
        "--",
        "-image",
        "echo",
        "--",
        "tail",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let info: serde_json::Value =
        serde_json::from_slice(&v(&["inspect", "dash-image"]).stdout).unwrap();
    assert_eq!(info[0]["config"]["image"], "-image");
    assert_eq!(
        info[0]["config"]["cmd"],
        serde_json::json!(["echo", "--", "tail"])
    );
}

#[test]
fn stopped_updates_patch_resources_mounts_and_sockets_without_resetting_omissions() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(
        v(&["volume", "create", "--size", "4M", "data"])
            .status
            .success()
    );
    let out = v(&[
        "create",
        "--name",
        "saved",
        "--cpus",
        "2",
        "-m",
        "128M",
        "-e",
        "A=first",
        "-e",
        "A=last",
        "-v",
        "data:/old",
        "--socket",
        "host=/tmp/host.sock,guest=/run/host.sock",
        "img",
        "true",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let before: serde_json::Value =
        serde_json::from_slice(&v(&["inspect", "saved"]).stdout).unwrap();
    let out = v(&[
        "update",
        "--memory",
        "256MiB",
        "--mount-remove",
        "/old",
        "--mount-add",
        "type=volume,source=data,target=/new",
        "--socket-remove",
        "/run/host.sock",
        "--socket-add",
        "host=/tmp/other.sock,guest=/run/new.sock",
        "saved",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let after: serde_json::Value =
        serde_json::from_slice(&v(&["inspect", "saved"]).stdout).unwrap();
    assert_eq!(after[0]["config"], before[0]["config"]);
    assert_eq!(after[0]["host_config"]["cpus"], 2);
    assert_eq!(after[0]["host_config"]["memory"], 256 << 20);
    assert_eq!(after[0]["host_config"]["mounts"][0]["target"], "/new");
    assert_eq!(
        after[0]["host_config"]["socket_bindings"][0]["guest_path"],
        "/run/new.sock"
    );
    assert!(
        v(&["update", "--mount-remove", "/new", "saved"])
            .status
            .success()
    );
    assert!(v(&["rm", "saved"]).status.success());
    assert!(v(&["volume", "inspect", "data"]).status.success());
}

#[test]
fn mount_spellings_normalize_equally_and_capability_limits_are_explicit() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    for (name, flags) in [
        ("short", vec!["-v", "data:/storage"]),
        (
            "typed",
            vec!["--mount", "type=volume,source=data,target=/storage"],
        ),
    ] {
        let mut args = vec!["create", "--name", name];
        args.extend(flags);
        args.extend(["img", "true"]);
        let out = v(&args);
        assert!(out.status.success(), "{}", text(&out.stderr));
    }
    let a: serde_json::Value =
        serde_json::from_slice(&v(&["inspect", "short", "typed"]).stdout).unwrap();
    assert_eq!(a[0]["host_config"]["mounts"], a[1]["host_config"]["mounts"]);
    let out = v(&[
        "create",
        "-v",
        "one:/a",
        "--mount",
        "type=volume,source=two,target=/b",
        "img",
        "true",
    ]);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out.stderr).contains("at most one named volume"));
    let out = v(&[
        "create",
        "-v",
        "data:/same",
        "--mount",
        "type=volume,source=data,target=/same",
        "img",
        "true",
    ]);
    assert!(!out.status.success());
    assert!(text(&out.stderr).contains("duplicate mount target"));
}

#[test]
fn exported_contract_and_help_are_deterministic_and_describe_public_rules() {
    let command = || {
        std::process::Command::new(env!("CARGO_BIN_EXE_vmctl"))
            .arg("--describe-contract")
            .output()
            .unwrap()
    };
    let a = command();
    let b = command();
    assert!(a.status.success(), "{}", text(&a.stderr));
    assert_eq!(a.stdout, b.stdout);
    let contract: serde_json::Value = serde_json::from_slice(&a.stdout).unwrap();
    assert_eq!(contract["exit_codes"]["invalid_invocation"], 2);
    assert_eq!(
        contract["schemas"]["machine"]["properties"]["created"]["type"],
        "integer"
    );
    let commands = contract["command"]["commands"].as_array().unwrap();
    for resource in ["machine", "image", "network", "volume"] {
        assert!(commands.iter().any(|c| c["name"] == resource));
    }
    for args in [
        vec!["machine", "create", "--help"],
        vec!["create", "--help"],
    ] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_vmctl"))
            .args(args)
            .output()
            .unwrap();
        let help = text(&out.stdout);
        assert!(out.status.success());
        assert!(
            help.contains("Host configuration:")
                && help.contains("Storage and sockets:")
                && help.contains("Networking:"),
            "{help}"
        );
        assert!(!help.contains("--state-dir") && !help.contains("--network-mode"));
    }
}

#[test]
fn json_invocation_errors_are_structured_before_any_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let out = vmctl(
        dir.path(),
        &guestd,
        &[
            "network", "create", "--output", "json", "--mode", "invalid", "team",
        ],
    );
    assert_eq!(out.status.code(), Some(2));
    let error: serde_json::Value = serde_json::from_slice(&out.stderr).unwrap();
    assert_eq!(error["origin"], "invocation");
    assert!(out.stdout.is_empty());
}

#[test]
fn cli_overrides_environment_and_collections_accumulate_without_resetting_updates() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let out = common::command(
        dir.path(),
        &guestd,
        &[
            "--state-dir",
            other.path().to_str().unwrap(),
            "create",
            "--name",
            "precedence",
            "-e",
            "KEY=first",
            "-e",
            "KEY=last",
            "img",
            "true",
        ],
    )
    .output()
    .unwrap();
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(
        vmctl(dir.path(), &guestd, &["inspect", "precedence"])
            .status
            .code(),
        Some(1)
    );
    let out = common::command(
        dir.path(),
        &guestd,
        &[
            "--state-dir",
            other.path().to_str().unwrap(),
            "inspect",
            "precedence",
        ],
    )
    .output()
    .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let entries = info[0]["config"]["env"].as_array().unwrap();
    assert_eq!(
        entries
            .iter()
            .rfind(|e| e.as_str().unwrap().starts_with("KEY="))
            .unwrap(),
        "KEY=last"
    );
    let out = vmctl(
        dir.path(),
        &guestd,
        &[
            "create",
            "--mount",
            "type=bind,source=/tmp,target=/a",
            "-v",
            "/tmp:/b:ro",
            "--mount",
            "type=bind,source=/tmp,target=/c",
            "--name",
            "mixed",
            "img",
            "true",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    let info: serde_json::Value =
        serde_json::from_slice(&vmctl(dir.path(), &guestd, &["inspect", "mixed"]).stdout).unwrap();
    let targets: Vec<_> = info[0]["host_config"]["mounts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["target"].as_str().unwrap())
        .collect();
    assert_eq!(targets, ["/a", "/b", "/c"]);
}

#[test]
fn foreground_output_is_byte_correct_without_opening_stdin() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
    let out = vmctl(
        dir.path(),
        &guestd,
        &[
            "machine",
            "run",
            "--name",
            "bytes",
            "img",
            "sh",
            "-c",
            "printf '\\377\\000out'; printf '\\376err' >&2; exit 7",
        ],
    );
    assert_eq!(out.status.code(), Some(7), "{}", text(&out.stderr));
    assert_eq!(out.stdout, b"\xff\0out");
    assert_eq!(out.stderr, b"\xfeerr");
    assert!(
        vmctl(dir.path(), &guestd, &["exec", "bytes", "true"])
            .status
            .success()
    );
}

#[test]
fn lists_use_shared_tables_and_typed_json_for_every_resource() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    for resource in ["machine", "image", "network", "volume"] {
        let invoke = |args: &[&str]| {
            common::command(dir.path(), &guestd, args)
                .env("CLANKERD_CACHE_DIR", dir.path().join("cache"))
                .output()
                .unwrap()
        };
        let human = invoke(&[resource, "ls"]);
        assert!(human.status.success(), "{}", text(&human.stderr));
        assert_eq!(text(&human.stdout).lines().count(), 1);
        let json = invoke(&[resource, "ls", "--output", "json"]);
        assert!(json.status.success());
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&json.stdout).unwrap(),
            serde_json::json!([])
        );
    }
}

#[test]
fn stopped_attachment_switching_clears_the_previous_alternative() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    assert!(v(&["network", "create", "team"]).status.success());
    assert!(
        v(&[
            "create",
            "--name",
            "saved",
            "--network",
            "team",
            "img",
            "true"
        ])
        .status
        .success()
    );
    assert!(
        v(&["update", "--bridge-interface", "en0", "saved"])
            .status
            .success()
    );
    let out = v(&["inspect", "saved"]);
    let info: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(info[0]["host_config"]["virtual_network"].is_null());
    assert!(info[0]["host_config"]["shared_ipv6_prefix"].is_null());
    assert_eq!(info[0]["host_config"]["network_mode"], "bridged");
    assert!(v(&["network", "rm", "team"]).status.success());
}

#[test]
fn metadata_shortcuts_preserve_result_schemas_and_effective_defaults() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .arg("--describe-contract")
        .output()
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let commands = metadata["command"]["commands"].as_array().unwrap();
    let machine = commands.iter().find(|c| c["name"] == "machine").unwrap();
    let actions = machine["commands"].as_array().unwrap();
    let canonical = actions.iter().find(|c| c["name"] == "ls").unwrap();
    let shortcut = commands.iter().find(|c| c["name"] == "ps").unwrap();
    assert_eq!(canonical["result"], shortcut["result"]);
    let create = actions.iter().find(|c| c["name"] == "create").unwrap();
    let arg = |command: &serde_json::Value, name: &str| {
        command["arguments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["name"] == name)
            .unwrap()
            .clone()
    };
    assert_eq!(arg(create, "pull")["effective_default"]["value"], "missing");
    assert_eq!(arg(create, "restart")["effective_default"]["value"], "no");
    assert_eq!(
        arg(create, "network")["effective_default"]["value"],
        "default-shared"
    );
    let update = actions.iter().find(|c| c["name"] == "update").unwrap();
    assert_eq!(
        arg(update, "pull")["effective_default"]["source"],
        "preserve_saved_value"
    );
    let volume = commands.iter().find(|c| c["name"] == "volume").unwrap();
    let create = volume["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "create")
        .unwrap();
    assert_eq!(
        arg(create, "size")["effective_default"]["value"],
        17179869184u64
    );
    assert!(
        metadata["schemas"]["machine"]["$defs"]["HostRecord"]["properties"]
            .get("shared_peer_participation")
            .is_none()
    );
    assert!(
        metadata["schemas"]["machine"]["$defs"]["ProbeRecord"]["properties"]
            .get("checked_at")
            .is_some()
    );
}
