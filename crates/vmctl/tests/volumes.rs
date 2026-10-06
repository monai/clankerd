//! CLI: `-v` volumes and bind mounts, `--volume-size`, `rm -v` and the
//! `volume` subcommands. The VMM is the local-process stand-in, so these check
//! what the CLI hands the library and what lands on the host.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const MIB: u64 = 1 << 20;

fn guestd() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["build", "-q", "-p", "clankerd-guestd"]);
    if profile_dir.file_name().is_some_and(|n| n == "release") {
        cmd.arg("--release");
    }
    assert!(cmd.status().unwrap().success());
    profile_dir.join("clankerd-guestd")
}

struct Cli {
    dir: tempfile::TempDir,
    guestd: PathBuf,
}

impl Cli {
    fn new() -> Self {
        Cli {
            dir: tempfile::Builder::new().prefix("vm").tempdir().unwrap(),
            guestd: guestd(),
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vmctl"))
            .args(args)
            .current_dir(self.root())
            .env("CLANKERD_STATE_DIR", self.root().join("state"))
            .env("CLANKERD_RUNTIME_DIR", self.root().join("run"))
            .env("CLANKERD_DEV_GUESTD", &self.guestd)
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
        self.root().join("state/volumes").join(name).join("data.ext4")
    }
}

#[test]
fn a_named_volume_is_created_sparse_with_the_requested_size_and_survives_rm() {
    let cli = Cli::new();
    cli.ok(&[
        "run", "--name", "a", "-v", "data:/storage", "--volume-size", "8M", "img", "true",
    ]);
    assert_eq!(
        std::fs::metadata(cli.volume_file("data")).unwrap().len(),
        8 * MIB
    );
    let listing = cli.ok(&["volume", "ls"]);
    assert!(listing.contains("data"), "{listing}");

    // Plain rm keeps the volume.
    cli.ok(&["rm", "a"]);
    assert!(cli.volume_file("data").exists());
    let inspect = cli.ok(&["volume", "inspect", "data"]);
    let v: serde_json::Value = serde_json::from_str(&inspect).unwrap();
    assert_eq!(v[0]["size"], 8 * MIB);

    // A new machine reuses it, and a bigger size grows it.
    cli.ok(&["run", "--name", "b", "-v", "data:/storage:size=24M", "img", "true"]);
    assert_eq!(
        std::fs::metadata(cli.volume_file("data")).unwrap().len(),
        24 * MIB
    );

    // rm -v deletes it.
    cli.ok(&["rm", "-v", "b"]);
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
    cli.ok(&["create", "--name", "a", "-v", "./proj:/workspace:ro", "img", "true"]);
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
        assert!(!out.status.success(), "{args:?} should fail");
    }
    assert!(cli.run(&["volume", "ls"]).status.success());
}
