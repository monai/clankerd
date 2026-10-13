//! Retired TCP tunnel flags are rejected at the CLI boundary.
use std::process::Command;

#[test]
fn tcp_tunnel_flags_are_rejected() {
    for flag in ["--publish", "-p", "--host-gateway-port"] {
        let output = Command::new(env!("CARGO_BIN_EXE_vmctl"))
            .args(["create", flag, "8080", "img", "true"])
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unexpected argument"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
