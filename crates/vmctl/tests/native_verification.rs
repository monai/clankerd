use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;
use test_support::process::{CommandOutcome, run_bounded};

struct NativeVerification {
    evidence: PathBuf,
    binary: PathBuf,
    sequence: std::cell::Cell<usize>,
    cleaned: std::cell::Cell<bool>,
}

impl NativeVerification {
    fn new() -> Self {
        assert_eq!(std::env::consts::OS, "macos", "requires native macOS");
        assert_eq!(std::env::consts::ARCH, "aarch64", "requires Apple Silicon");
        let out = std::env::var_os("OUT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../build/artifact")
            });
        let binary = out.join("darwin-arm64/vmctl");
        assert!(binary.is_file(), "build the signed native artifact first");
        let evidence = tempfile::Builder::new()
            .prefix("clankerd-verification-")
            .tempdir_in("/tmp")
            .unwrap()
            .keep();
        eprintln!("verification evidence retained at {}", evidence.display());
        let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let metadata = |name: &str, args: &[&str]| {
            let outcome = run_bounded(
                Command::new("git").args(args).current_dir(&source),
                &evidence.join(name),
                Duration::from_secs(10),
            )
            .expect("record checkout metadata");
            assert!(
                matches!(outcome, CommandOutcome::Exited(status) if status.success()),
                "git metadata failed; evidence {}",
                evidence.display()
            );
            fs::read(evidence.join(format!("{name}.stdout"))).unwrap()
        };
        let revision = metadata("revision", &["rev-parse", "HEAD"]);
        let patch = metadata("patch", &["diff", "--binary", "HEAD"]);
        metadata("status", &["status", "--porcelain"]);
        fs::write(evidence.join("source.patch"), &patch).unwrap();
        fs::write(evidence.join("manifest.json"), serde_json::to_vec_pretty(&serde_json::json!({
            "journey": "lifecycle", "source_revision": String::from_utf8_lossy(&revision).trim(),
            "source_patch_sha256": Sha256::digest(&patch).iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            "binary": binary, "binary_sha256": Sha256::digest(fs::read(&binary).unwrap()).iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            "image": std::env::var("IMAGE").unwrap_or_else(|_| "ghcr.io/monai/clankers:slim".into())
        })).unwrap()).unwrap();
        Self {
            cleaned: std::cell::Cell::new(false),
            evidence,
            binary,
            sequence: std::cell::Cell::new(0),
        }
    }

    fn run(&self, args: &[&str]) -> bool {
        let sequence = self.sequence.get();
        self.sequence.set(sequence + 1);
        let prefix = self.evidence.join(format!("{sequence:03}"));
        if let Err(error) = fs::write(
            prefix.with_extension("command.json"),
            serde_json::to_vec(args).unwrap(),
        ) {
            eprintln!("cannot record {args:?}: {error}");
            return false;
        }
        let outcome = run_bounded(
            Command::new(&self.binary)
                .args(args)
                .env("CLANKERD_STATE_DIR", self.evidence.join("state"))
                .env("CLANKERD_RUNTIME_DIR", self.evidence.join("run"))
                .env_remove("CLANKERD_DEV_GUESTD")
                .env_remove("CLANKERD_VMSPAWN_DEV_LOCAL")
                .env_remove("CLANKERD_BOOT_DIR_ROOT"),
            &prefix,
            Duration::from_secs(180),
        );
        let success = matches!(&outcome, Ok(CommandOutcome::Exited(status)) if status.success());
        let _ = fs::write(prefix.with_extension("result"), format!("{outcome:?}"));
        success
    }

    fn ok(&self, args: &[&str]) -> String {
        assert!(
            self.run(args),
            "{args:?} failed; evidence {}",
            self.evidence.display()
        );
        fs::read_to_string(
            self.evidence
                .join(format!("{:03}.stdout", self.sequence.get() - 1)),
        )
        .unwrap()
        .trim()
        .to_owned()
    }

    fn cleanup(&self) -> bool {
        self.run(&["machine", "logs", "example"]);
        let success = self.run(&["machine", "rm", "-f", "example"]);
        self.cleaned.set(success);
        let _ = fs::write(
            self.evidence.join("cleanup.json"),
            format!("{{\"success\":{success}}}"),
        );
        if !success {
            eprintln!(
                "cleanup required:\nexport CLANKERD_STATE_DIR={}\nexport CLANKERD_RUNTIME_DIR={}\n{} machine rm -f example",
                shell_quote(&self.evidence.join("state")),
                shell_quote(&self.evidence.join("run")),
                shell_quote(&self.binary)
            );
        }
        success
    }
}

fn shell_quote(path: &std::path::Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

impl Drop for NativeVerification {
    fn drop(&mut self) {
        if !self.cleaned.get() {
            self.cleanup();
        }
    }
}

#[test]
#[ignore = "draft native journey; requires signed Apple Silicon artifact and registry access"]
fn lifecycle_preserves_machine_after_workload_exit_and_disk_across_boots() {
    let native = NativeVerification::new();
    native.ok(&["--version"]);
    let image = std::env::var("IMAGE").unwrap_or_else(|_| "ghcr.io/monai/clankers:slim".into());
    native.ok(&[
        "machine",
        "run",
        "--name",
        "example",
        "--entrypoint",
        "/bin/sh",
        &image,
        "-c",
        "printf example > /tmp/verification-value",
    ]);
    assert_eq!(
        native.ok(&[
            "machine",
            "exec",
            "example",
            "sh",
            "-ec",
            "cat /tmp/verification-value"
        ]),
        "example",
        "exec remains available after workload completion"
    );
    native.ok(&[
        "machine",
        "exec",
        "example",
        "sh",
        "-ec",
        "printf persisted > /tmp/verification-persisted",
    ]);
    native.ok(&["machine", "stop", "example"]);
    native.ok(&["machine", "start", "example"]);
    assert_eq!(
        native.ok(&[
            "machine",
            "exec",
            "example",
            "sh",
            "-ec",
            "cat /tmp/verification-persisted"
        ]),
        "persisted",
        "root disk persists across stop and start"
    );
    assert!(native.cleanup(), "owned machine removal succeeds");
}
