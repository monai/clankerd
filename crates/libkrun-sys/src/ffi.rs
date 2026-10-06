//! Raw declarations of the libkrun 1.19.x C API (`include/libkrun.h`) that
//! clankerd uses. Keep `stubs/libkrun.tbd` in sync when adding a function.

use std::ffi::c_char;

unsafe extern "C" {
    pub fn krun_set_log_level(level: u32) -> i32;
    pub fn krun_create_ctx() -> i32;
    pub fn krun_set_vm_config(ctx_id: u32, num_vcpus: u8, ram_mib: u32) -> i32;
    /// Root of the guest: a host directory shared over virtio-fs.
    pub fn krun_set_root(ctx_id: u32, root_path: *const c_char) -> i32;
    /// `argv` and `envp` are NULL-terminated arrays of C strings.
    pub fn krun_set_exec(
        ctx_id: u32,
        exec_path: *const c_char,
        argv: *const *const c_char,
        envp: *const *const c_char,
    ) -> i32;
    /// Attaches a raw disk image as a virtio-blk device (`/dev/vda`, `/dev/vdb`,
    /// ... in call order). `disk_format` 0 is raw.
    pub fn krun_add_disk2(
        ctx_id: u32,
        block_id: *const c_char,
        disk_path: *const c_char,
        disk_format: u32,
        read_only: bool,
    ) -> i32;
    pub fn krun_set_console_output(ctx_id: u32, c_filepath: *const c_char) -> i32;
    /// With `listen`, libkrun binds a unix socket at `c_filepath` and forwards
    /// connections to the guest's vsock `port`.
    pub fn krun_add_vsock_port2(
        ctx_id: u32,
        port: u32,
        c_filepath: *const c_char,
        listen: bool,
    ) -> i32;
    /// Takes over the process; returns only when setup fails.
    pub fn krun_start_enter(ctx_id: u32) -> i32;
}
