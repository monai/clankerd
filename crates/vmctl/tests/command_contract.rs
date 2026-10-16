mod common;
use common::{guestd, text, vmctl};

#[test]
fn canonical_machine_hierarchy_and_shortcuts_share_required_operands() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    for action in [
        "create", "run", "start", "stop", "kill", "update", "inspect", "rm", "exec", "logs",
        "reimage",
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
    for resource in ["network", "volume"] {
        for action in ["create", "inspect", "rm"] {
            let out = vmctl(dir.path(), &guestd, &[resource, action]);
            assert_eq!(
                out.status.code(),
                Some(2),
                "{resource} {action}: {}",
                text(&out.stderr)
            );
        }
    }
}

#[test]
fn partial_inspection_emits_successes_in_input_order_and_reports_each_failed_target() {
    for resource in ["machine", "network", "volume"] {
        let dir = tempfile::tempdir().unwrap();
        let guestd = guestd();
        let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
        for name in ["first", "last"] {
            let mut args = vec![resource, "create"];
            if resource == "machine" {
                args.extend(["--name", name, "img", "true"]);
            } else {
                args.push(name);
            }
            let out = v(&args);
            assert!(out.status.success(), "{}", text(&out.stderr));
        }
        let out = v(&[
            resource,
            "inspect",
            "--output",
            "json",
            "missing",
            "last",
            "first",
            "other-missing",
        ]);
        assert_eq!(out.status.code(), Some(1));
        let records: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(records.as_array().unwrap().len(), 2);
        assert_eq!(records[0]["name"], "last");
        assert_eq!(records[1]["name"], "first");
        let errors: Vec<serde_json::Value> = text(&out.stderr)
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(errors.len(), 2);
        for (error, target) in errors.iter().zip(["missing", "other-missing"]) {
            assert_eq!(error["target"], target);
            assert_eq!(error["origin"], "operation");
            assert_eq!(error["kind"], "NotFound");
        }
    }
}

#[test]
fn invalid_invocations_are_rejected_before_execution() {
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
}

#[test]
fn guest_tails_and_end_of_options_preserve_literal_arguments() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
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
fn named_volume_spellings_normalize_identically() {
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
}

#[test]
fn multiple_named_volume_spellings_preserve_collection_order() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
    let out = v(&[
        "create",
        "--name",
        "multi",
        "-v",
        "one:/a",
        "--mount",
        "type=volume,source=two,target=/b",
        "img",
        "true",
    ]);
    assert!(out.status.success(), "{}", text(&out.stderr));
    let inspected = v(&["inspect", "multi"]);
    let machines: serde_json::Value = serde_json::from_slice(&inspected.stdout).unwrap();
    assert_eq!(machines[0]["host_config"]["mounts"][0]["name"], "one");
    assert_eq!(machines[0]["host_config"]["mounts"][1]["name"], "two");
}

#[test]
fn mount_targets_are_unique_across_spellings() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
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
fn exported_contract_is_deterministic_and_describes_public_resources() {
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
}

#[test]
fn action_help_groups_options_and_excludes_global_flags() {
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
fn explicit_global_values_override_environment() {
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
    assert_eq!(info[0]["name"], "precedence");
}

#[test]
fn mixed_mount_spellings_preserve_input_order() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
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
fn metadata_shortcuts_share_canonical_result_contracts() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .arg("--describe-contract")
        .output()
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let commands = metadata["command"]["commands"].as_array().unwrap();
    for (shortcut, resource, action) in [
        ("create", "machine", "create"),
        ("run", "machine", "run"),
        ("ps", "machine", "ls"),
        ("inspect", "machine", "inspect"),
        ("start", "machine", "start"),
        ("stop", "machine", "stop"),
        ("kill", "machine", "kill"),
        ("update", "machine", "update"),
        ("reimage", "machine", "reimage"),
        ("rm", "machine", "rm"),
        ("exec", "machine", "exec"),
        ("logs", "machine", "logs"),
        ("pull", "image", "pull"),
        ("images", "image", "ls"),
    ] {
        let group = commands.iter().find(|c| c["name"] == resource).unwrap();
        let canonical = group["commands"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == action)
            .unwrap();
        let alias = commands.iter().find(|c| c["name"] == shortcut).unwrap();
        assert_eq!(alias["result"], canonical["result"], "{shortcut}");
        assert_eq!(alias["arguments"], canonical["arguments"], "{shortcut}");
    }
}

#[test]
fn metadata_declares_effective_defaults_separately_from_omitted_updates() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .arg("--describe-contract")
        .output()
        .unwrap();
    let metadata: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let commands = metadata["command"]["commands"].as_array().unwrap();
    let machine = commands.iter().find(|c| c["name"] == "machine").unwrap();
    let actions = machine["commands"].as_array().unwrap();
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
}

#[test]
fn repeated_environment_entries_use_the_last_value_for_each_key_in_the_guest() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
    let out = vmctl(
        dir.path(),
        &guestd,
        &[
            "run",
            "-e",
            "KEY=first",
            "-e",
            "OTHER=retained",
            "-e",
            "KEY=last",
            "img",
            "sh",
            "-c",
            "printf '%s %s' \"$KEY\" \"$OTHER\"",
        ],
    );
    assert!(out.status.success(), "{}", text(&out.stderr));
    assert_eq!(out.stdout, b"last retained");
}
