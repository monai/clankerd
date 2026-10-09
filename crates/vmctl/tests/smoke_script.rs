//! scripts/m2-smoke-test.sh against a stub `vmctl` (and `curl`) on Linux: the
//! real run needs the M2, but the script's flow, failure reporting and
//! cleanup are checked here. The stub logs each invocation and fails any whose
//! arguments contain $STUB_FAIL_ON.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/m2-smoke-test.sh")
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct Run {
    out: Output,
    calls: String,
}

fn run_script(fail_on: Option<&str>) -> Run {
    let dir = tempfile::Builder::new().prefix("smoke").tempdir().unwrap();
    let bin = dir.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let log = dir.path().join("calls.log");
    executable(
        &bin.join("vmctl"),
        r#"#!/bin/sh
echo "$*" >> "$STUB_LOG"
case "$*" in "exec -i "*) cat; exit 0 ;; esac
case "$*" in *"$STUB_FAIL_ON"*) [ -n "$STUB_FAIL_ON" ] && { echo "stub: failing $*" >&2; exit 1; } ;; esac
exit 0
"#,
    );
    executable(&bin.join("curl"), "#!/bin/sh\necho 'Welcome to nginx!'\n");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let out = Command::new("bash")
        .arg(script())
        .env("PATH", path)
        .env("VMCTL", bin.join("vmctl"))
        .env("STUB_LOG", &log)
        .env("STUB_FAIL_ON", fail_on.unwrap_or(""))
        .env("SMOKE_HOSTGW_PORT", "18931")
        .env("SMOKE_TIMEOUT", "3")
        .output()
        .unwrap();
    let calls = fs::read_to_string(&log).unwrap_or_default();
    Run { out, calls }
}

#[test]
fn a_clean_run_walks_the_whole_prototype_and_cleans_up() {
    let r = run_script(None);
    let stderr = String::from_utf8_lossy(&r.out.stderr);
    let stdout = String::from_utf8_lossy(&r.out.stdout);
    assert!(r.out.status.success(), "{stdout}\n{stderr}\n{}", r.calls);
    assert!(stdout.contains("PASS"), "{stdout}");
    for expected in [
        "pull ghcr.io/monai/clankers:slim",
        "-p 8080:8080",
        "--host-gateway-port 18931",
        "-v smoke-data:/storage",
        "stop smoke",
        "start smoke",
    ] {
        assert!(
            r.calls.contains(expected),
            "missing `{expected}` in:\n{}",
            r.calls
        );
    }
    assert!(
        r.calls.contains("volume rm smoke-data"),
        "volume not cleaned up:\n{}",
        r.calls
    );
}

#[test]
fn nftables_table_failure_is_reported_with_the_kata_fallback() {
    let r = run_script(Some("nft list tables"));
    assert!(!r.out.status.success());
    let stderr = String::from_utf8_lossy(&r.out.stderr);
    assert!(stderr.contains("FAIL"), "{stderr}");
    assert!(stderr.contains("Kata"), "{stderr}");
    assert!(
        r.calls.contains("exec -u root smoke sh -c nft list tables"),
        "{}",
        r.calls
    );
    assert!(
        r.calls.contains("rm -f -v smoke"),
        "a failed run still cleans up:\n{}",
        r.calls
    );
}

#[test]
fn a_failed_stop_start_check_fails_the_run() {
    let r = run_script(Some("persisted"));
    assert!(!r.out.status.success());
    assert!(String::from_utf8_lossy(&r.out.stderr).contains("FAIL"));
}
