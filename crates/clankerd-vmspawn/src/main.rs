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
    run(&spec, listener, hypervisor.as_ref())
}
