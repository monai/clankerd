//! What the developer reads when libkrun, the kernel library or the
//! hypervisor is not usable. These cases are reproducible on Linux; the real
//! calls are only reachable on macOS.

use libkrun_sys::{Error, ErrorKind};

#[test]
fn missing_hypervisor_entitlement_names_the_entitlement_and_the_fix() {
    // HV_DENIED, as returned by Hypervisor.framework's hv_vm_create.
    let e = Error::from_hv_return(0xfae9_4007);
    assert_eq!(e.kind(), ErrorKind::EntitlementMissing);
    let msg = e.to_string();
    assert!(msg.contains("com.apple.security.hypervisor"), "{msg}");
    assert!(msg.contains("make rust-sign"), "{msg}");
}

#[test]
fn hypervisor_unavailable_is_distinct_from_the_entitlement() {
    for code in [0xfae9_4006, 0xfae9_400f] {
        let e = Error::from_hv_return(code);
        assert_eq!(e.kind(), ErrorKind::HypervisorUnavailable, "{code:#x}");
        assert!(e.to_string().contains("Hypervisor.framework"), "{e}");
    }
}

#[test]
fn unknown_hypervisor_codes_are_reported_in_hex() {
    let e = Error::from_hv_return(0xfae9_4001);
    assert_eq!(e.kind(), ErrorKind::HypervisorUnavailable);
    assert!(e.to_string().contains("0xfae94001"), "{e}");
}

#[test]
fn missing_libkrunfw_says_how_to_install_it() {
    // krun_start_enter returns -ENOENT when libkrunfw cannot be loaded.
    let e = Error::from_krun("krun_start_enter", -2);
    assert_eq!(e.kind(), ErrorKind::KernelLibraryMissing);
    assert!(e.to_string().contains("brew install libkrunfw"), "{e}");
}

#[test]
fn other_libkrun_failures_name_the_call_and_errno() {
    let e = Error::from_krun("krun_set_root", -22);
    assert_eq!(e.kind(), ErrorKind::Libkrun);
    let msg = e.to_string();
    assert!(
        msg.contains("krun_set_root") && msg.contains("Invalid argument"),
        "{msg}"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn booting_is_unsupported_off_macos() {
    let cfg = libkrun_sys::BootConfig::new("/boot", "/clankerd-guestd");
    let e = libkrun_sys::boot(&cfg).unwrap_err();
    assert_eq!(e.kind(), ErrorKind::Unsupported);
    assert!(e.to_string().contains("macOS"), "{e}");
}
