//! Seam B harness: the real clankerd-guestd binary, driven only through its
//! varlink interface over a unix socket (standing in for vsock). Nothing is faked.
#![allow(dead_code)]

use std::io::{BufReader, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use clankerd_proto::varlink::{self, Call, Reply};
use serde::Serialize;

pub mod tarbuild;

pub fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions.
    unsafe { libc::geteuid() == 0 }
}

/// Prints a skip notice that shows up in `cargo test -- --nocapture` and in CI logs.
pub fn skip(test: &str, why: &str) {
    eprintln!("SKIPPED {test}: {why}");
}

/// Returns early from the test, with a clear message, unless running as root.
#[macro_export]
macro_rules! require_root {
    () => {
        if !common::is_root() {
            common::skip(
                module_path!(),
                "needs root (run: sudo -E cargo test -p clankerd-guestd --test seam_b)",
            );
            return;
        }
    };
}

pub struct Guestd {
    child: Child,
    socket: PathBuf,
    _dir: tempfile::TempDir,
}

impl Drop for Guestd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Guestd {
    /// Starts guestd in `--populate` mode; `boot_dir` holds mke2fs.
    pub fn spawn(boot_dir: &Path) -> Self {
        let dir = tempfile::Builder::new().prefix("gd").tempdir().unwrap();
        let socket = dir.path().join("g.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let fd = listener.as_raw_fd();
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_clankerd-guestd"));
        cmd.args(["--populate", "--boot-dir"])
            .arg(boot_dir)
            .env("LISTEN_FDS", "1")
            .stdin(Stdio::null());
        // SAFETY: only async-signal-safe calls between fork and exec.
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
        let child = cmd.spawn().unwrap();
        Guestd {
            child,
            socket,
            _dir: dir,
        }
    }

    /// Plain call: one request, one reply.
    pub fn call<P: Serialize>(&self, method: &str, params: &P) -> Reply {
        self.call_with_stream(method, params, &mut std::io::empty())
    }

    /// Stream-in call: the request, then `stream` until EOF, then half-close.
    pub fn call_with_stream<P: Serialize>(
        &self,
        method: &str,
        params: &P,
        stream: &mut dyn std::io::Read,
    ) -> Reply {
        let mut conn = UnixStream::connect(&self.socket).unwrap();
        varlink::write(
            &mut conn,
            &Call {
                method: method.into(),
                parameters: serde_json::to_value(params).unwrap(),
                more: false,
            },
        )
        .unwrap();
        // The reply may arrive early (errors); ignore EPIPE while streaming.
        let _ = std::io::copy(stream, &mut conn);
        let _ = conn.flush();
        let _ = conn.shutdown(std::net::Shutdown::Write);
        varlink::read::<Reply, _>(&mut BufReader::new(conn))
            .unwrap()
            .expect("guestd closed without replying")
    }
}

/// A boot directory containing the host's mke2fs, standing in for the shipped
/// static binary (CI and developers have e2fsprogs; the static build is
/// verified separately by scripts/rust-verify.sh).
pub fn boot_dir_with_mke2fs() -> tempfile::TempDir {
    let dir = tempfile::Builder::new().prefix("boot").tempdir().unwrap();
    let mke2fs = ["/usr/sbin/mke2fs", "/sbin/mke2fs", "/usr/bin/mke2fs"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .expect("mke2fs (e2fsprogs) must be installed to run seam B tests");
    std::os::unix::fs::symlink(mke2fs, dir.path().join("mke2fs")).unwrap();
    dir
}
