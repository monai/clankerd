//! clankerd-guestd: guest-side agent. In this tracer bullet it runs the workload
//! and serves `io.clankerd.Guest.Events` on a socket inherited via `LISTEN_FDS`
//! (a unix socket standing in for vsock). Boot-level setup (pivot, mounts) comes later.
//!
//! Usage: `clankerd-guestd --config WORKLOAD.json [--exit-file PATH]
//! [--vsock-port N] [--host-socket PATH | --host-vsock-port N] [--loopback ADDR]`
//!
//! The listening socket is either inherited via `LISTEN_FDS` (local-process
//! stand-in) or an `AF_VSOCK` listener on `--vsock-port` (real VM). The host
//! tunnel endpoint is a unix socket (`--host-socket`) or a vsock port on the
//! host (`--host-vsock-port`). `--loopback` shifts the guest loopback address
//! for the local stand-in VMM.
//!
//! The optional exit file receives the workload's exit code so the result
//! survives the death of whoever was subscribed. Inside a VM the boot directory
//! is read-only by convention, so the host side (clankerd-vmspawn) records it
//! from the `Exited` event instead.
//!
//! As PID 1 (booted by libkrun's init.krun) guestd powers the machine off once
//! the workload has ended and subscribers have seen the result.

mod tunnel;
mod vsock;

use std::io::BufReader;
use std::os::fd::FromRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use clankerd_proto::guest::{
    ERROR_INVALID_PARAMETER, ERROR_METHOD_NOT_FOUND, Event, METHOD_EVENTS, METHOD_EXEC_CREATE,
    METHOD_EXEC_INSPECT, METHOD_EXEC_KILL, METHOD_EXEC_RESIZE, METHOD_EXEC_START, Workload,
};
use clankerd_proto::varlink::{self, Call, Reply};
use serde_json::Value;

mod exec;
mod user;

const LISTEN_FD: i32 = 3;
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// How long to keep serving after the workload exits so subscribers see `exited`.
const LINGER: Duration = Duration::from_millis(300);
const DRAIN_LIMIT: Duration = Duration::from_secs(5);

struct Shared {
    inner: Mutex<Inner>,
    cv: Condvar,
    execs: exec::Registry,
}

#[derive(Default)]
struct Inner {
    exit_code: Option<i32>,
    streams: usize,
}

fn main() {
    let mut config = None;
    let mut exit_file = None;
    let mut host_socket = None;
    let mut host_vsock_port = None;
    let mut vsock_port = None;
    let mut loopback = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--config" => config = args.next().map(PathBuf::from),
            "--exit-file" => exit_file = args.next().map(PathBuf::from),
            "--host-socket" => host_socket = args.next().map(PathBuf::from),
            "--host-vsock-port" => {
                host_vsock_port = Some(port_arg(args.next(), "--host-vsock-port"))
            }
            "--vsock-port" => vsock_port = Some(port_arg(args.next(), "--vsock-port")),
            "--loopback" => loopback = args.next(),
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
    let host = match (host_socket, host_vsock_port) {
        (Some(path), _) => Some(tunnel::HostEndpoint::Unix(path)),
        (None, Some(port)) => Some(tunnel::HostEndpoint::Vsock(port)),
        (None, None) => None,
    };

    let loopback = loopback
        .map(|l| {
            l.parse()
                .unwrap_or_else(|_| die("invalid --loopback address"))
        })
        .unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]));
    let tunnels = Arc::new(tunnel::Tunnels::new(loopback, host));

    let shared = Arc::new(Shared {
        inner: Mutex::default(),
        cv: Condvar::new(),
        execs: exec::Registry::new(workload.env.clone(), workload.working_dir.clone()),
    });
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
        let tunnels = tunnels.clone();
        std::thread::spawn(move || serve(conn, shared, tunnels));
    }
}

fn port_arg(value: Option<String>, flag: &str) -> u32 {
    value
        .and_then(|v| v.parse().ok())
        .unwrap_or_else(|| die(&format!("{flag} needs a port number")))
}

/// The listening socket; connections are served identically either way.
enum Listener {
    Unix(UnixListener),
    Vsock(std::os::fd::OwnedFd),
}

impl Listener {
    fn accept(&self) -> Option<UnixStream> {
        match self {
            Listener::Unix(l) => l.accept().ok().map(|(s, _)| s),
            Listener::Vsock(fd) => {
                let conn = vsock::accept(fd);
                if conn.is_none() {
                    std::thread::sleep(Duration::from_millis(10));
                }
                conn
            }
        }
    }
}

fn die(msg: &str) -> ! {
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

fn serve(conn: UnixStream, shared: Arc<Shared>, tunnels: Arc<tunnel::Tunnels>) {
    let Ok(mut out) = conn.try_clone() else {
        return;
    };
    let mut input = BufReader::new(conn);
    let Ok(Some(call)) = varlink::read::<Call, _>(&mut input) else {
        return;
    };
    if tunnel::handles(&call.method) {
        tunnels.handle(call, input, out);
        return;
    }
    match call.method.as_str() {
        METHOD_EVENTS => {
            shared.inner.lock().unwrap().streams += 1;
            let _ = events(&mut out, &shared);
            end_stream(&shared);
        }
        METHOD_EXEC_START => exec_start(input, out, call.parameters, &shared),
        METHOD_EXEC_CREATE | METHOD_EXEC_RESIZE | METHOD_EXEC_KILL | METHOD_EXEC_INSPECT => {
            let result = exec_call(&call, &shared.execs);
            let _ = reply(&mut out, result);
        }
        _ => {
            let _ = reply(&mut out, Err((ERROR_METHOD_NOT_FOUND, String::new())));
        }
    }
}

fn end_stream(shared: &Shared) {
    shared.inner.lock().unwrap().streams -= 1;
    shared.cv.notify_all();
}

fn reply(out: &mut UnixStream, result: Result<Value, exec::Failure>) -> std::io::Result<()> {
    let reply = match result {
        Ok(parameters) => Reply {
            parameters,
            continues: false,
            error: None,
        },
        Err((name, message)) => Reply {
            parameters: serde_json::json!({ "message": message }),
            continues: false,
            error: Some(name.into()),
        },
    };
    varlink::write(out, &reply)
}

fn params<T: serde::de::DeserializeOwned>(call: &Call) -> Result<T, exec::Failure> {
    serde_json::from_value(call.parameters.clone())
        .map_err(|e| (ERROR_INVALID_PARAMETER, e.to_string()))
}

fn exec_call(call: &Call, execs: &exec::Registry) -> Result<Value, exec::Failure> {
    fn json<T: serde::Serialize>(v: &T) -> Value {
        serde_json::to_value(v).unwrap()
    }
    let empty = || serde_json::json!({});
    match call.method.as_str() {
        METHOD_EXEC_CREATE => Ok(json(&execs.create(params(call)?)?)),
        METHOD_EXEC_RESIZE => execs.resize(&params(call)?).map(|()| empty()),
        METHOD_EXEC_KILL => execs.kill(&params(call)?).map(|()| empty()),
        _ => Ok(json(&execs.inspect(&params(call)?)?)),
    }
}

/// Upgrades the connection and runs the exec as a framed stream.
fn exec_start(input: BufReader<UnixStream>, mut out: UnixStream, p: Value, shared: &Arc<Shared>) {
    let handle = serde_json::from_value::<clankerd_proto::guest::ExecRef>(p)
        .map_err(|e| (ERROR_INVALID_PARAMETER, e.to_string()))
        .and_then(|r| shared.execs.claim(&r.id));
    match handle {
        Err(e) => {
            let _ = reply(&mut out, Err(e));
        }
        Ok(handle) => {
            if reply(&mut out, Ok(serde_json::json!({}))).is_err() {
                return;
            }
            shared.inner.lock().unwrap().streams += 1;
            handle.run(input, out);
            end_stream(shared);
        }
    }
}

fn events(out: &mut UnixStream, shared: &Shared) -> std::io::Result<()> {
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
