//! clankerd-guestd: guest-side agent. In this tracer bullet it runs the workload
//! and serves `io.clankerd.Guest.Events` on a socket inherited via `LISTEN_FDS`
//! (a unix socket standing in for vsock). Boot-level setup (pivot, mounts) comes later.
//!
//! Usage: `clankerd-guestd --config WORKLOAD.json [--exit-file PATH] [--vsock-port N]`
//!
//! The listening socket is either inherited via `LISTEN_FDS` (local-process
//! stand-in) or an `AF_VSOCK` listener on `--vsock-port` (real VM).
//!
//! The optional exit file receives the workload's exit code so the result
//! survives the death of whoever was subscribed. Inside a VM the boot directory
//! is read-only, so the host side (clankerd-vmspawn) records it from the
//! `Exited` event instead.
//!
//! As PID 1 (booted by libkrun's init.krun) guestd powers the machine off once
//! the workload has ended and subscribers have seen the result.

use std::io::BufReader;
use std::os::fd::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use clankerd_proto::guest::{ERROR_METHOD_NOT_FOUND, Event, METHOD_EVENTS, Workload};
use clankerd_proto::varlink::{self, Call, Reply};

const LISTEN_FD: i32 = 3;
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// How long to keep serving after the workload exits so subscribers see `exited`.
const LINGER: Duration = Duration::from_millis(300);
const DRAIN_LIMIT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct Shared {
    inner: Mutex<Inner>,
    cv: Condvar,
}

#[derive(Default)]
struct Inner {
    exit_code: Option<i32>,
    streams: usize,
}

fn main() {
    let mut config = None;
    let mut exit_file = None;
    let mut vsock_port = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config = args.next().map(PathBuf::from),
            "--exit-file" => exit_file = args.next().map(PathBuf::from),
            "--vsock-port" => {
                vsock_port = args
                    .next()
                    .and_then(|p| p.parse::<u32>().ok())
                    .or_else(|| die("--vsock-port needs a port number"));
            }
            other => die(&format!("unknown argument {other}")),
        }
    }
    let config = config.unwrap_or_else(|| die("--config is required"));
    let workload: Workload = std::fs::read(&config)
        .map_err(|e| e.to_string())
        .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
        .unwrap_or_else(|e| die(&format!("reading {}: {e}", config.display())));

    let listener = if std::env::var("LISTEN_FDS").ok().as_deref() == Some("1") {
        // SAFETY: fd 3 is the listening socket handed to us by our parent.
        Listener::Unix(unsafe { UnixListener::from_raw_fd(LISTEN_FD) })
    } else if let Some(port) = vsock_port {
        Listener::Vsock(
            vsock::listen(port)
                .unwrap_or_else(|e| die(&format!("cannot listen on vsock port {port}: {e}"))),
        )
    } else {
        die("no socket to listen on: set LISTEN_FDS=1 with a socket on fd 3, or pass --vsock-port")
    };

    let shared = Arc::new(Shared::default());
    let child = spawn_workload(&workload);
    {
        let shared = shared.clone();
        std::thread::spawn(move || supervise(child, shared, exit_file));
    }

    loop {
        let Some(conn) = listener.accept() else {
            continue;
        };
        let shared = shared.clone();
        std::thread::spawn(move || serve(conn, shared));
    }
}

/// The listening socket; connections are served identically either way.
enum Listener {
    Unix(UnixListener),
    Vsock(std::os::fd::OwnedFd),
}

impl Listener {
    fn accept(&self) -> Option<Conn> {
        match self {
            Listener::Unix(l) => l.accept().ok().map(|(s, _)| Conn::Unix(s)),
            Listener::Vsock(fd) => {
                use std::os::fd::AsRawFd;
                // SAFETY: accept on a valid listening fd; peer address not needed.
                let c = unsafe {
                    libc::accept(fd.as_raw_fd(), std::ptr::null_mut(), std::ptr::null_mut())
                };
                if c < 0 {
                    std::thread::sleep(Duration::from_millis(10));
                    return None;
                }
                // SAFETY: c is a fresh fd we own.
                Some(Conn::Vsock(unsafe { std::os::fd::OwnedFd::from_raw_fd(c) }))
            }
        }
    }
}

/// An accepted connection as a readable and writable file.
enum Conn {
    Unix(UnixStream),
    Vsock(std::os::fd::OwnedFd),
}

impl Conn {
    fn into_file(self) -> std::fs::File {
        match self {
            Conn::Unix(s) => std::fs::File::from(std::os::fd::OwnedFd::from(s)),
            Conn::Vsock(fd) => std::fs::File::from(fd),
        }
    }
}

mod vsock {
    use std::os::fd::{FromRawFd, OwnedFd};

    /// Listens on `port` for any CID.
    pub fn listen(port: u32) -> std::io::Result<OwnedFd> {
        // SAFETY: plain socket calls; the fd is wrapped immediately.
        unsafe {
            let fd = libc::socket(libc::AF_VSOCK, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0);
            if fd < 0 {
                return Err(std::io::Error::last_os_error());
            }
            let fd = OwnedFd::from_raw_fd(fd);
            let mut addr: libc::sockaddr_vm = std::mem::zeroed();
            addr.svm_family = libc::AF_VSOCK as libc::sa_family_t;
            addr.svm_cid = libc::VMADDR_CID_ANY;
            addr.svm_port = port;
            let rc = libc::bind(
                std::os::fd::AsRawFd::as_raw_fd(&fd),
                (&raw const addr).cast(),
                std::mem::size_of::<libc::sockaddr_vm>() as libc::socklen_t,
            );
            if rc < 0 || libc::listen(std::os::fd::AsRawFd::as_raw_fd(&fd), 16) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(fd)
        }
    }
}

fn die<T>(msg: &str) -> T {
    eprintln!("clankerd-guestd: {msg}");
    std::process::exit(2)
}

/// Spawns the workload; a failure to exec maps to Docker's 127 / 126 exit codes.
fn spawn_workload(w: &Workload) -> Result<std::process::Child, i32> {
    let Some((prog, rest)) = w.argv.split_first() else {
        eprintln!("clankerd-guestd: no command to run");
        return Err(127);
    };
    let mut cmd = Command::new(prog);
    cmd.args(rest).stdin(Stdio::null()).env_clear();
    cmd.env("PATH", DEFAULT_PATH);
    for kv in &w.env {
        if let Some((k, v)) = kv.split_once('=') {
            cmd.env(k, v);
        }
    }
    if !w.working_dir.is_empty() {
        cmd.current_dir(&w.working_dir);
    }
    cmd.spawn().map_err(|e| {
        eprintln!("clankerd-guestd: cannot run {prog}: {e}");
        if e.kind() == std::io::ErrorKind::NotFound {
            127
        } else {
            126
        }
    })
}

/// Waits for the workload, records its result, then exits once subscribers have seen it.
fn supervise(
    child: Result<std::process::Child, i32>,
    shared: Arc<Shared>,
    exit_file: Option<PathBuf>,
) {
    let code = match child {
        Ok(mut c) => match c.wait() {
            Ok(s) => s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
            Err(_) => 255,
        },
        Err(code) => code,
    };
    if let Some(exit_file) = exit_file {
        let tmp = exit_file.with_extension("tmp");
        if std::fs::write(&tmp, code.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &exit_file);
        }
    }
    shared.inner.lock().unwrap().exit_code = Some(code);
    shared.cv.notify_all();

    std::thread::sleep(LINGER);
    let deadline = Instant::now() + DRAIN_LIMIT;
    let mut g = shared.inner.lock().unwrap();
    while g.streams > 0 && Instant::now() < deadline {
        g = shared
            .cv
            .wait_timeout(g, Duration::from_millis(50))
            .unwrap()
            .0;
    }
    drop(g);
    if std::process::id() == 1 {
        power_off();
    }
    std::process::exit(0);
}

/// Flushes and powers the VM off; as PID 1 returning would panic the kernel.
fn power_off() -> ! {
    // SAFETY: sync and reboot take no pointers.
    unsafe {
        libc::sync();
        libc::reboot(libc::RB_POWER_OFF);
    }
    // reboot only returns on failure; PID 1 must not exit.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn serve(conn: Conn, shared: Arc<Shared>) {
    let conn = conn.into_file();
    let Ok(mut out) = conn.try_clone() else {
        return;
    };
    let mut input = BufReader::new(conn);
    let Ok(Some(call)) = varlink::read::<Call, _>(&mut input) else {
        return;
    };
    if call.method != METHOD_EVENTS {
        let _ = varlink::write(
            &mut out,
            &Reply {
                parameters: serde_json::Value::Null,
                continues: false,
                error: Some(ERROR_METHOD_NOT_FOUND.into()),
            },
        );
        return;
    }
    shared.inner.lock().unwrap().streams += 1;
    let _ = events(&mut out, &shared);
    shared.inner.lock().unwrap().streams -= 1;
    shared.cv.notify_all();
}

fn events(out: &mut std::fs::File, shared: &Shared) -> std::io::Result<()> {
    let reply = |event: &Event, continues| Reply {
        parameters: serde_json::to_value(event).unwrap(),
        continues,
        error: None,
    };
    varlink::write(out, &reply(&Event::Ready, true))?;
    let mut g = shared.inner.lock().unwrap();
    let code = loop {
        if let Some(code) = g.exit_code {
            break code;
        }
        g = shared.cv.wait(g).unwrap();
    };
    drop(g);
    varlink::write(out, &reply(&Event::Exited { exit_code: code }, false))
}
