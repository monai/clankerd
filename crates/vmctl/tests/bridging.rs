mod common;

use common::{guestd, text, vmctl};

#[test]
fn bridged_cli_requires_an_explicit_interface_and_rejects_shared_only_options() {
    let dir = tempfile::tempdir().unwrap();
    let guestd = guestd();
    for flags in [
        vec!["--network-mode", "bridged"],
        vec!["--network-mode", "shared", "--bridge-interface", "en0"],
        vec!["--bridge-interface", "en0", "--isolate-peers"],
        vec![
            "--bridge-interface",
            "en0",
            "--shared-network",
            "192.168.109.0/24",
        ],
    ] {
        let mut args = vec!["create"];
        args.extend(flags);
        args.extend(["img", "true"]);
        let result = vmctl(dir.path(), &guestd, &args);
        assert!(!result.status.success());
        let result = vmctl(dir.path(), &guestd, &["ps", "--all"]);
        assert!(
            !text(&result.stdout).contains("img"),
            "invalid network must not create a machine"
        );
    }
}
