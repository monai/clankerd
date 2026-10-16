//! CLI: typed and shorthand mounts, explicit volume sizing/removal and the
//! `volume` subcommands. The VMM is the local-process stand-in, so these check
//! what the CLI hands the library and what lands on the host.

mod common;

use std::path::{Path, PathBuf};
use std::process::Output;

use common::{command, guestd};

const MIB: u64 = 1 << 20;

struct Cli {
    dir: tempfile::TempDir,
    guestd: PathBuf,
}

impl Drop for Cli {
    fn drop(&mut self) {
        drop(common::cleanup_on_drop(self.dir.path(), &self.guestd));
    }
}

impl Cli {
    fn new() -> Self {
        Cli {
            dir: tempfile::Builder::new().prefix("vm").tempdir().unwrap(),
            guestd: guestd(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        command(self.root(), &self.guestd, args)
            .current_dir(self.root())
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn volume_file(&self, name: &str) -> PathBuf {
        self.root()
            .join("state/volumes")
            .join(name)
            .join("data.ext4")
    }
}

#[test]
fn a_named_volume_is_created_sparse_with_the_requested_size_and_survives_rm() {
    let cli = Cli::new();
    cli.ok(&["volume", "create", "--size", "8M", "data"]);
    cli.ok(&["run", "--name", "a", "-v", "data:/storage", "img", "true"]);
    assert_eq!(
        std::fs::metadata(cli.volume_file("data")).unwrap().len(),
        8 * MIB
    );
    let listing = cli.ok(&["volume", "ls"]);
    assert!(listing.contains("data"), "{listing}");

    // Plain rm keeps the volume.
    cli.ok(&["stop", "a"]);
    cli.ok(&["rm", "a"]);
    assert!(cli.volume_file("data").exists());
    let inspect = cli.ok(&["volume", "inspect", "data"]);
    let v: serde_json::Value = serde_json::from_str(&inspect).unwrap();
    assert_eq!(v[0]["size"], 8 * MIB);

    // A new machine reuses the existing volume without resizing it.
    cli.ok(&["run", "--name", "b", "-v", "data:/storage", "img", "true"]);
    assert_eq!(
        std::fs::metadata(cli.volume_file("data")).unwrap().len(),
        8 * MIB
    );

    // Explicit volume removal deletes it after the machine is removed.
    cli.ok(&["stop", "b"]);
    cli.ok(&["rm", "b"]);
    assert!(cli.volume_file("data").exists());
    cli.ok(&["volume", "rm", "data"]);
    assert!(!cli.volume_file("data").exists());
}

#[test]
fn volume_subcommands_create_and_remove() {
    let cli = Cli::new();
    cli.ok(&["volume", "create", "--size", "4M", "scratch"]);
    assert_eq!(
        std::fs::metadata(cli.volume_file("scratch")).unwrap().len(),
        4 * MIB
    );
    cli.ok(&["volume", "rm", "scratch"]);
    assert!(!cli.volume_file("scratch").exists());
    assert!(!cli.run(&["volume", "rm", "scratch"]).status.success());
}

#[test]
fn a_relative_host_directory_becomes_an_absolute_bind_mount() {
    let cli = Cli::new();
    std::fs::create_dir(cli.root().join("proj")).unwrap();
    cli.ok(&[
        "create",
        "--name",
        "a",
        "-v",
        "./proj:/workspace:ro",
        "img",
        "true",
    ]);
    let v: serde_json::Value = serde_json::from_str(&cli.ok(&["inspect", "a"])).unwrap();
    let m = &v[0]["host_config"]["mounts"][0];
    assert_eq!(m["type"], "bind");
    assert_eq!(m["target"], "/workspace");
    assert_eq!(m["read_only"], true);
    let source = PathBuf::from(m["source"].as_str().unwrap());
    assert!(source.is_absolute());
    assert_eq!(
        source.canonicalize().unwrap(),
        cli.root().join("proj").canonicalize().unwrap()
    );

    let missing = cli.run(&["create", "-v", "./nope:/x", "img", "true"]);
    assert!(!missing.status.success());
}

#[test]
fn malformed_volume_flags_are_rejected() {
    let cli = Cli::new();
    for args in [
        &["create", "-v", "data", "img", "true"][..],
        &["create", "-v", "data:storage", "img", "true"],
        &["create", "-v", "data:/s:size=lots", "img", "true"],
        &["create", "-v", "data:/s:bogus", "img", "true"],
        &["create", "--volume-size", "1G", "img", "true"],
    ] {
        let out = cli.run(args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            common::text(&out.stderr)
        );
    }
    assert!(cli.run(&["volume", "ls"]).status.success());
}

#[test]
fn typed_mount_fields_require_values_and_have_unique_known_keys() {
    let cli = Cli::new();
    for mount in [
        "type=volume,source,target=/storage",
        "type,source=data,target=/storage",
        "type=volume,source=data,target",
        "type=volume,source=,target=/storage",
        "type=volume,target=/storage",
        "type=volume,source=data",
        "type=volume,source=data,source=other,target=/storage",
        "type=volume,source=data,target=/storage,unknown=value",
        "type=bind,source=/tmp,target=/storage,readonly=maybe",
    ] {
        let out = cli.run(&["create", "--mount", mount, "img", "true"]);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{mount}: {}",
            common::text(&out.stderr)
        );
        assert!(out.stdout.is_empty());
    }
    let machines: serde_json::Value =
        serde_json::from_str(&cli.ok(&["machine", "ls", "--all", "--output", "json"])).unwrap();
    assert_eq!(machines, serde_json::json!([]));
}

#[test]
fn bind_spellings_resolve_paths_and_access_to_the_same_attachment() {
    let cli = Cli::new();
    std::fs::create_dir(cli.root().join("proj")).unwrap();
    std::fs::create_dir(cli.root().join("child")).unwrap();
    let absolute = cli.root().join("proj").canonicalize().unwrap();
    for (index, source, cwd) in [
        (0, "./proj".to_owned(), cli.root().to_owned()),
        (1, "../proj".to_owned(), cli.root().join("child")),
        (2, "~/proj".to_owned(), cli.root().to_owned()),
        (3, absolute.display().to_string(), cli.root().to_owned()),
    ] {
        for readonly in [false, true] {
            for typed in [false, true] {
                let name = format!("case-{index}-{readonly}-{typed}");
                let value = if typed {
                    format!("type=bind,source={source},target=/shared,readonly={readonly}")
                } else {
                    format!("{source}:/shared:{}", if readonly { "ro" } else { "rw" })
                };
                let out = command(
                    cli.root(),
                    &cli.guestd,
                    &[
                        "create",
                        "--name",
                        &name,
                        if typed { "--mount" } else { "--volume" },
                        &value,
                        "img",
                        "true",
                    ],
                )
                .current_dir(&cwd)
                .env("HOME", cli.root())
                .output()
                .unwrap();
                assert!(
                    out.status.success(),
                    "{value}: {}",
                    common::text(&out.stderr)
                );
                let info: serde_json::Value =
                    serde_json::from_str(&cli.ok(&["inspect", &name])).unwrap();
                assert_eq!(
                    info[0]["host_config"]["mounts"],
                    serde_json::json!([{"type":"bind","source":absolute,"target":"/shared","read_only":readonly}])
                );
            }
        }
    }
}

#[test]
fn read_only_access_is_rejected_for_named_volume_attachments() {
    let cli = Cli::new();
    for args in [
        vec!["create", "-v", "data:/storage:ro", "img", "true"],
        vec![
            "create",
            "--mount",
            "type=volume,source=data,target=/storage,readonly",
            "img",
            "true",
        ],
        vec![
            "create",
            "--mount",
            "type=volume,source=data,target=/storage,readonly=true",
            "img",
            "true",
        ],
    ] {
        let out = cli.run(&args);
        assert_eq!(
            out.status.code(),
            Some(2),
            "{args:?}: {}",
            common::text(&out.stderr)
        );
        assert!(out.stdout.is_empty());
        assert!(common::text(&out.stderr).contains("read-only"));
    }
}
