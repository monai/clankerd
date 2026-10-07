//! CLI: `--host-gateway-port` makes a host loopback port reachable from the
//! guest's loopback (ticket 10 smoke test needs it). The VMM is the
//! local-process stand-in, so this checks what the CLI hands the library.

mod common;

use common::{guestd, vmctl};

#[test]
fn host_gateway_ports_are_recorded_in_the_machine_config() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let v = |args: &[&str]| vmctl(dir.path(), &guestd, args);

    let created = v(&[
        "create",
        "--name",
        "gw",
        "--host-gateway-port",
        "9000",
        "--host-gateway-port",
        "9001",
        "img",
        "true",
    ]);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let inspect: serde_json::Value =
        serde_json::from_slice(&v(&["inspect", "gw"]).stdout).expect("inspect prints JSON");
    let text = inspect.to_string();
    assert!(
        text.contains("\"host_gateway_ports\":[9000,9001]"),
        "{text}"
    );
}

#[test]
fn host_gateway_port_zero_is_rejected() {
    let dir = tempfile::Builder::new().prefix("vm").tempdir().unwrap();
    let guestd = guestd();
    let out = vmctl(
        dir.path(),
        &guestd,
        &["create", "--host-gateway-port", "0", "img", "true"],
    );
    assert!(!out.status.success());
}
