//! The `slim` image changes handed to the developer (contrib/image/slim):
//! daemon.json and the helper the entrypoint calls. dockerd is a stub, so this
//! checks the contract: only inside a clankerd machine, with the right
//! configuration, and failing loudly when dockerd never comes up.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn slim_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../contrib/image/slim")
}

#[test]
fn daemon_json_selects_nftables_and_puts_data_on_the_volume() {
    let text = fs::read_to_string(slim_dir().join("daemon.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["firewall-backend"], "nftables");
    assert_eq!(v["data-root"], "/storage/docker");
}

/// A sandbox with a stub dockerd that records its arguments and, like the real
/// one, creates its unix socket (here a plain file) when it is "ready".
struct Sandbox {
    dir: tempfile::TempDir,
}

impl Sandbox {
    fn new(dockerd_body: &str) -> Self {
        let dir = tempfile::Builder::new().prefix("img").tempdir().unwrap();
        let stub = dir.path().join("dockerd");
        fs::write(&stub, format!("#!/bin/sh\n{dockerd_body}\n")).unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o755)).unwrap();
        Sandbox { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn start(&self, marker_present: bool) -> Output {
        if marker_present {
            fs::write(self.path("clankerdenv"), "").unwrap();
        }
        Command::new("sh")
            .arg(slim_dir().join("clankerd-dockerd"))
            .env("CLANKERDENV", self.path("clankerdenv"))
            .env("DOCKERD", self.path("dockerd"))
            .env("DOCKER_SOCK", self.path("docker.sock"))
            .env("DOCKERD_LOG", self.path("dockerd.log"))
            .env("DOCKERD_WAIT", "3")
            .env("DOCKERD_NO_SUDO", "1")
            .env("ARGS_FILE", self.path("args"))
            .output()
            .unwrap()
    }
}

const READY_DOCKERD: &str = r#"echo "$@" > "$ARGS_FILE"; : > "$DOCKER_SOCK"; sleep 5"#;

#[test]
fn dockerd_starts_when_the_clankerdenv_marker_exists() {
    let sb = Sandbox::new(READY_DOCKERD);
    let out = sb.start(true);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(sb.path("docker.sock").exists());
    let args = fs::read_to_string(sb.path("args")).unwrap();
    assert!(
        args.contains("--config-file=/etc/docker/daemon.json"),
        "{args}"
    );
}

#[test]
fn nothing_starts_without_the_marker() {
    let sb = Sandbox::new(READY_DOCKERD);
    let out = sb.start(false);
    assert!(out.status.success());
    assert!(
        !sb.path("args").exists(),
        "dockerd ran outside a clankerd machine"
    );
}

#[test]
fn a_dockerd_that_never_becomes_ready_fails_with_its_log() {
    let sb = Sandbox::new(r#"echo "nftables: not supported" >&2; sleep 5"#);
    let out = sb.start(true);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("nftables: not supported"), "{stderr}");
}
