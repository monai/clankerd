//! Convenience spellings obey the same management and process contracts.
mod common;
use common::{guestd, text, vmctl};
use serde_json::Value;

#[test]
fn every_shortcut_shares_help_and_invocation_errors_with_its_canonical_command() {
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
        let invoke = |args: &[&str]| {
            std::process::Command::new(env!("CARGO_BIN_EXE_vmctl"))
                .args(args)
                .output()
                .unwrap()
        };
        let canonical = invoke(&[resource, action, "--help"]);
        let alias = invoke(&[shortcut, "--help"]);
        assert!(canonical.status.success());
        assert!(alias.status.success());
        assert_eq!(alias.stdout, canonical.stdout, "{shortcut}");
        let canonical = invoke(&[resource, action, "--unknown-option"]);
        let alias = invoke(&[shortcut, "--unknown-option"]);
        assert_eq!(canonical.status.code(), Some(2));
        assert_eq!(alias.status.code(), Some(2));
        assert_eq!(alias.stderr, canonical.stderr, "{shortcut}");
    }
}

#[test]
fn machine_shortcuts_and_canonical_commands_have_the_same_observable_effects() {
    for shortcut in [
        "create", "run", "ps", "inspect", "start", "stop", "kill", "update", "rm", "exec", "logs",
    ] {
        for canonical in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let guestd = guestd();
            let _cleanup = common::cleanup_on_drop(dir.path(), &guestd);
            let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);
            let running = matches!(shortcut, "ps" | "stop" | "kill" | "exec" | "logs");
            if !matches!(shortcut, "create" | "run") {
                let setup = if running {
                    vec![
                        "machine", "run", "-d", "--name", "saved", "img", "printf", "example",
                    ]
                } else {
                    vec!["machine", "create", "--name", "saved", "img", "true"]
                };
                let out = v(&setup);
                assert!(out.status.success(), "{}", text(&out.stderr));
            }
            let action = if shortcut == "ps" { "ls" } else { shortcut };
            let mut args = if canonical {
                vec!["machine", action]
            } else {
                vec![shortcut]
            };
            args.extend(match shortcut {
                "create" => vec!["--name", "saved", "img", "true"],
                "run" => vec!["-d", "--name", "saved", "img", "true"],
                "ps" => vec!["--output", "json"],
                "update" => vec!["--memory", "256M", "saved"],
                "exec" => vec!["saved", "printf", "%s", "literal;$HOME"],
                _ => vec!["saved"],
            });
            let out = v(&args);
            assert_eq!(
                out.status.code(),
                Some(0),
                "{shortcut}/{canonical}: {}",
                text(&out.stderr)
            );
            if matches!(shortcut, "create" | "run") {
                assert_eq!(text(&out.stdout).trim().len(), 64);
            }
            if matches!(shortcut, "start" | "stop" | "kill" | "update" | "rm") {
                assert_eq!(text(&out.stdout), "saved\n");
            }
            if shortcut == "exec" {
                assert_eq!(out.stdout, b"literal;$HOME");
            }
            if shortcut == "logs" {
                common::wait_for("console output", || {
                    text(&v(&args).stdout).contains("example")
                });
            }
            if matches!(shortcut, "ps" | "inspect") {
                let records: Value = serde_json::from_slice(&out.stdout).unwrap();
                assert_eq!(records[0]["name"], "saved");
            }
            let inspected = v(&["machine", "inspect", "saved"]);
            if shortcut == "rm" {
                assert_eq!(inspected.status.code(), Some(1));
                continue;
            }
            assert!(inspected.status.success());
            let info: Value = serde_json::from_slice(&inspected.stdout).unwrap();
            let status = match shortcut {
                "create" | "inspect" | "update" => "created",
                "stop" | "kill" => "exited",
                _ => "running",
            };
            assert_eq!(info[0]["state"]["status"], status);
            if shortcut == "update" {
                assert_eq!(info[0]["host_config"]["memory"], 268435456);
            }
            if shortcut == "kill" {
                assert_eq!(info[0]["state"]["exit_code"], 137);
            }
        }
    }
}

#[test]
fn existing_target_shortcuts_share_operation_failures() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    for action in [
        "inspect", "start", "stop", "kill", "update", "reimage", "rm", "exec", "logs",
    ] {
        for canonical in [false, true] {
            let mut args = if canonical {
                vec!["machine", action]
            } else {
                vec![action]
            };
            if action == "update" {
                args.extend(["--memory", "256M"]);
            }
            args.push("missing");
            if matches!(action, "exec" | "reimage") {
                args.push("true");
            }
            let out = vmctl(dir.path(), &guestd, &args);
            assert_eq!(
                out.status.code(),
                Some(1),
                "{args:?}: {}",
                text(&out.stderr)
            );
            assert!(text(&out.stderr).contains("no such machine"));
        }
    }
}
