//! clankerd-vmspawn: one helper process per machine; runs libkrun.
//!
//! Usage: `clankerd-vmspawn --spec SPEC.json [--dev-local]`
//!
//! The listening socket for the machine's guestd endpoint arrives on fd 3
//! (`LISTEN_FDS=1`). libkrun takes over this process and exits it when the
//! guest powers off, so the helper outlives whoever started it. Failures are
//! printed to stderr, which the starter collects in `vmspawn.log`.
//!
//! `--dev-local` (hidden) runs the boot directory's guestd as a local process
//! instead of booting a VM, for development and tests on Linux.

use std::os::fd::FromRawFd;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;

use clankerd_proto::spawn::SpawnSpec;
use clankerd_vmspawn::{DevLocal, Hypervisor, Libkrun, run};

const LISTEN_FD: i32 = 3;

fn main() -> ExitCode {
    match real_main() {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("clankerd-vmspawn: {msg}");
            ExitCode::from(1)
        }
    }
}

fn real_main() -> Result<(), String> {
    let mut spec_path = None;
    let mut dev_local = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--spec" => spec_path = args.next().map(PathBuf::from),
            "--dev-local" => dev_local = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let spec_path = spec_path.ok_or("--spec is required")?;
    let spec: SpawnSpec = std::fs::read(&spec_path)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        .map_err(|e| format!("reading {}: {e}", spec_path.display()))?;

    if std::env::var("LISTEN_FDS").ok().as_deref() != Some("1") {
        return Err("expected one socket via LISTEN_FDS".into());
    }
    // SAFETY: fd 3 is the listening socket handed to us by our starter.
    let listener = unsafe { UnixListener::from_raw_fd(LISTEN_FD) };

    let hypervisor: Box<dyn Hypervisor> = if dev_local {
        Box::new(DevLocal)
    } else {
        Box::new(Libkrun)
    };
    #[cfg(target_os = "macos")]
    load_firmware()?;
    reserve_descriptor_capacity()?;
    run(&spec, listener, hypervisor.as_ref())
}

fn reserve_descriptor_capacity() -> Result<(), String> {
    // The VMM owns virtio-fs pipes, vsock listeners and transient management
    // connections. macOS commonly inherits a soft limit of only 256.
    // Raise this process alone, preserving its hard limit and any larger soft limit.
    // SAFETY: initialized writable rlimit storage is passed to getrlimit.
    let mut limits: libc::rlimit = unsafe { std::mem::zeroed() };
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limits) } != 0 {
        return Err(format!(
            "reading VMM open-file limit: {}",
            std::io::Error::last_os_error()
        ));
    }
    let desired = limits.rlim_cur.max(4096.min(limits.rlim_max));
    if desired != limits.rlim_cur {
        limits.rlim_cur = desired;
        // SAFETY: limits preserves the inherited hard bound and soft <= hard.
        if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &limits) } != 0 {
            return Err(format!(
                "reserving VMM descriptor capacity: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    eprintln!(
        "clankerd-vmspawn: open-file limit soft={} hard={}",
        limits.rlim_cur, limits.rlim_max
    );
    Ok(())
}

#[cfg(target_os = "macos")]
fn load_firmware() -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt;
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let path = executable
        .parent()
        .ok_or("missing executable directory")?
        .join("lib/libkrunfw.5.dylib");
    let path = std::ffi::CString::new(path.as_os_str().as_bytes()).map_err(|e| e.to_string())?;
    // SAFETY: load the bundled firmware and retain it for libkrun's lifetime.
    if unsafe { libc::dlopen(path.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) }.is_null() {
        return Err("cannot load bundled libkrunfw.5.dylib".into());
    }
    Ok(())
}
