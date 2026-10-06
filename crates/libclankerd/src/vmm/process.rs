use std::fs::{self, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use super::{BootHandle, BootSpec, Vmm};
use crate::error::{Error, Result};

/// Runs clankerd-guestd as a local process. The listening socket is created
/// here and handed over with the `LISTEN_FDS` convention; the process gets its
/// own session so it outlives the library, like a real VMM helper.
#[derive(Debug, Clone)]
pub struct LocalProcessVmm {
    guestd: PathBuf,
}

impl LocalProcessVmm {
    /// The stand-in guest's loopback address. The guest shares the host's
    /// network namespace here, so its loopback is shifted to keep guest
    /// services from colliding with host services on the same port.
    pub const GUEST_LOOPBACK: &'static str = "127.0.0.2";

    pub fn new(guestd: impl Into<PathBuf>) -> Self {
        LocalProcessVmm {
            guestd: guestd.into(),
        }
    }
}

impl Vmm for LocalProcessVmm {
    fn boot(&self, spec: &BootSpec) -> Result<BootHandle> {
        let config = spec.dir.join("workload.json");
        fs::write(&config, serde_json::to_vec(&spec.workload)?)?;
        let console = OpenOptions::new()
            .create(true)
            .append(true)
            .open(spec.dir.join("console.log"))?;

        let _ = fs::remove_file(&spec.guest_socket);
        let listener = UnixListener::bind(&spec.guest_socket)
            .map_err(|e| Error::system(format!("binding {}: {e}", spec.guest_socket.display())))?;
        let fd = listener.as_raw_fd();

        let mut cmd = Command::new(&self.guestd);
        cmd.arg("--config")
            .arg(&config)
            .arg("--exit-file")
            .arg(&spec.exit_file)
            .arg("--host-socket")
            .arg(&spec.host_socket)
            .arg("--loopback")
            .arg(Self::GUEST_LOOPBACK)
            .env("LISTEN_FDS", "1")
            .stdin(Stdio::null())
            .stdout(console.try_clone()?)
            .stderr(console)
            .process_group(0);
        // SAFETY: only async-signal-safe calls (dup2, fcntl) between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                if fd == 3 {
                    let flags = libc::fcntl(3, libc::F_GETFD);
                    libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                } else if libc::dup2(fd, 3) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| {
            Error::unavailable(format!("cannot start {}: {e}", self.guestd.display()))
        })?;
        let pid = child.id();
        // Reap it if it ends while we are still alive.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(BootHandle { pid })
    }
}
