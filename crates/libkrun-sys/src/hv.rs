//! Probe for a usable hypervisor and the entitlement, before libkrun would
//! crash on its own `hv_vm_create` failure.
//!
//! Hypervisor.framework is loaded with dlopen so linking needs no SDK.

use std::ffi::{CStr, c_void};

use crate::Error;

const FRAMEWORK: &CStr = c"/System/Library/Frameworks/Hypervisor.framework/Hypervisor";

type VmCreate = unsafe extern "C" fn(config: *mut c_void) -> u32;
type VmDestroy = unsafe extern "C" fn() -> u32;

/// Creates and destroys a VM in this process, mapping failure to a clear error.
pub fn probe() -> Result<(), Error> {
    // SAFETY: dlopen/dlsym on a system framework; the symbols have the
    // documented signatures `hv_return_t hv_vm_create(hv_vm_config_t)` and
    // `hv_return_t hv_vm_destroy(void)`.
    unsafe {
        let lib = libc::dlopen(FRAMEWORK.as_ptr(), libc::RTLD_NOW);
        if lib.is_null() {
            return Err(Error::from_hv_return(0xfae9_400f));
        }
        let create = libc::dlsym(lib, c"hv_vm_create".as_ptr());
        let destroy = libc::dlsym(lib, c"hv_vm_destroy".as_ptr());
        if create.is_null() || destroy.is_null() {
            return Err(Error::from_hv_return(0xfae9_400f));
        }
        let create: VmCreate = std::mem::transmute(create);
        let destroy: VmDestroy = std::mem::transmute(destroy);
        let rc = create(std::ptr::null_mut());
        if rc != 0 {
            return Err(Error::from_hv_return(rc));
        }
        destroy();
    }
    Ok(())
}
