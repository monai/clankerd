//! Real Mac virtio-fs contracts. Build signed helpers with `make rust`, then run:
//! cargo test -p vmctl --test native_bind_semantics -- --ignored --test-threads=1
//! OUT selects the helper artifacts; IMAGE selects a Linux image with sh, stat,
//! chmod, chown, mount, touch and sleep (default: the native smoke-test image).

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;
use std::process::{Command, Output};

struct NativeShare {
    dir: tempfile::TempDir,
    out: PathBuf,
}

impl NativeShare {
    fn new(semantics: &str, read_only: bool) -> Self {
        assert_eq!(
            std::env::consts::OS,
            "macos",
            "requires an Apple Silicon Mac"
        );
        assert_eq!(
            std::env::consts::ARCH,
            "aarch64",
            "requires an Apple Silicon Mac"
        );
        let out = std::env::var_os("OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/rust"));
        for binary in [
            "darwin-arm64/clankerd-vmspawn",
            "linux-arm64/clankerd-guestd",
        ] {
            assert!(
                out.join(binary).is_file(),
                "missing {binary}; run make rust"
            );
        }
        let share = Self {
            dir: tempfile::Builder::new()
                .prefix("bind")
                .tempdir_in("/tmp")
                .unwrap(),
            out,
        };
        fs::create_dir(share.dir.path().join("shared")).unwrap();
        fs::set_permissions(
            share.dir.path().join("shared"),
            fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        fs::write(share.file(), "original\n").unwrap();
        fs::set_permissions(share.file(), fs::Permissions::from_mode(0o644)).unwrap();
        let mount = format!(
            "type=bind,source={},target=/shared,semantics={semantics},readonly={read_only}",
            share.dir.path().join("shared").display()
        );
        let image = std::env::var("IMAGE").unwrap_or_else(|_| "ghcr.io/monai/clankers:slim".into());
        share.ok(&[
            "run",
            "-d",
            "--name",
            "example",
            "--user",
            "root",
            "--entrypoint",
            "/bin/sh",
            "--mount",
            &mount,
            &image,
            "-c",
            "exec sleep 3600",
        ]);
        share.exec("root", "for tool in stat chmod chown mount touch; do command -v \"$tool\" >/dev/null || exit 1; done");
        share
    }

    fn file(&self) -> PathBuf {
        self.dir.path().join("shared/file")
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(self.out.join("darwin-arm64/vmctl"))
            .args(args)
            .env("CLANKERD_STATE_DIR", self.dir.path().join("state"))
            .env("CLANKERD_RUNTIME_DIR", self.dir.path().join("run"))
            .env_remove("CLANKERD_DEV_GUESTD")
            .env_remove("CLANKERD_VMSPAWN_DEV_LOCAL")
            .env_remove("CLANKERD_BOOT_DIR_ROOT")
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
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn exec(&self, user: &str, script: &str) -> String {
        self.ok(&["exec", "--user", user, "example", "sh", "-ec", script])
    }

    fn restart(&self) {
        self.ok(&["stop", "example"]);
        self.ok(&["start", "example"]);
    }
}

impl Drop for NativeShare {
    fn drop(&mut self) {
        if std::thread::panicking() {
            let logs = self.run(&["logs", "example"]);
            eprintln!("{}", String::from_utf8_lossy(&logs.stdout));
        }
        let _ = self.run(&["rm", "-f", "example"]);
    }
}

#[test]
#[ignore = "requires Apple Silicon, signed native helpers, and registry access"]
fn simplified_ownership_follows_each_requesting_user() {
    let share = NativeShare::new("simplified", false);
    for user in ["1000:100", "2000:200", "1000:100"] {
        assert_eq!(share.exec(user, "stat -c '%u:%g' /shared/file"), user);
    }
}

#[test]
#[ignore = "requires Apple Silicon, signed native helpers, and registry access"]
fn simplified_chown_does_not_establish_a_persistent_owner() {
    let share = NativeShare::new("simplified", false);
    let before = fs::metadata(share.file()).unwrap();
    share.exec("root", "chown 2000:200 /shared/file");
    assert_eq!(
        share.exec("1000:100", "stat -c '%u:%g' /shared/file"),
        "1000:100"
    );
    let after = fs::metadata(share.file()).unwrap();
    assert_eq!((after.uid(), after.gid()), (before.uid(), before.gid()));
}

#[test]
#[ignore = "requires Apple Silicon, signed native helpers, and registry access"]
fn simplified_chmod_changes_actual_host_permissions() {
    let share = NativeShare::new("simplified", false);
    share.exec("root", "chmod 600 /shared/file");
    assert_eq!(fs::metadata(share.file()).unwrap().mode() & 0o777, 0o600);
}

#[test]
#[ignore = "requires Apple Silicon, signed native helpers, and registry access"]
fn complete_ownership_is_stable_and_survives_restart() {
    let share = NativeShare::new("complete", false);
    let host = fs::metadata(share.file()).unwrap();
    share.exec("root", "chown 1000:100 /shared/file");
    for user in ["1000:100", "2000:200"] {
        assert_eq!(share.exec(user, "stat -c '%u:%g' /shared/file"), "1000:100");
    }
    share.restart();
    assert_eq!(
        share.exec("root", "stat -c '%u:%g' /shared/file"),
        "1000:100"
    );
    let after = fs::metadata(share.file()).unwrap();
    assert_eq!((after.uid(), after.gid()), (host.uid(), host.gid()));
}

#[test]
#[ignore = "requires Apple Silicon, signed native helpers, and registry access"]
fn complete_permissions_persist_without_changing_host_permissions() {
    let share = NativeShare::new("complete", false);
    share.exec("root", "chmod 600 /shared/file");
    assert_eq!(share.exec("root", "stat -c '%a' /shared/file"), "600");
    share.restart();
    assert_eq!(share.exec("root", "stat -c '%a' /shared/file"), "600");
    assert_eq!(fs::metadata(share.file()).unwrap().mode() & 0o777, 0o644);
}

#[test]
#[ignore = "requires Apple Silicon, signed native helpers, and registry access"]
fn writable_shares_allow_writes_after_a_guest_remount_in_both_modes() {
    for semantics in ["simplified", "complete"] {
        let share = NativeShare::new(semantics, false);
        share.exec(
            "root",
            "mount -o remount,rw /shared; printf changed > /shared/file; touch /shared/new",
        );
        assert_eq!(fs::read_to_string(share.file()).unwrap(), "changed");
        assert!(share.dir.path().join("shared/new").is_file());
    }
}

#[test]
#[ignore = "requires Apple Silicon, signed native helpers, and registry access"]
fn read_only_shares_reject_mutations_even_after_a_guest_remount() {
    for semantics in ["simplified", "complete"] {
        let share = NativeShare::new(semantics, true);
        share.exec(
            "root",
            r#"
            mount -o remount,rw /shared || true
            if printf changed > /shared/file; then echo 'overwrote read-only file' >&2; exit 1; fi
            if touch /shared/new; then echo 'created read-only file' >&2; exit 1; fi
            if chmod 600 /shared/file; then echo 'changed read-only permissions' >&2; exit 1; fi
            owner=$(stat -c '%u:%g' /shared/file)
            chown 1234:1234 /shared/file 2>/dev/null || true
            test "$(stat -c '%u:%g' /shared/file)" = "$owner"
        "#,
        );
        assert_eq!(fs::read_to_string(share.file()).unwrap(), "original\n");
        assert!(!share.dir.path().join("shared/new").exists());
        assert_eq!(fs::metadata(share.file()).unwrap().mode() & 0o777, 0o644);
    }
}
