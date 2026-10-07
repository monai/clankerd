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
//! Population boot (`--populate`, no workload): serves only the root-disk
//! methods (format, unpack tar, populate disk) with the static e2fsprogs from
//! `--boot-dir` (default: next to this binary); `--lenient-ownership` is a
//! development flag for runs without root.
//!
//! As PID 1 (booted by libkrun's init.krun) guestd powers the machine off once
//! the workload has ended and subscribers have seen the result: it signals
//! and reaps every process, syncs, unmounts and powers off (`power`).
//!
//! With `--boot` guestd first mounts the root disk (`/dev/vda`), pivots into
//! it, mounts /proc, /sys, /dev and cgroup2, delegates cgroup controllers,
//! sets the clock from the host and writes `/.clankerdenv` (`boot`).

mod attach;
mod boot;
mod network;
mod poll;
mod power;
mod rootdisk;
mod tunnel;
mod unpack;
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
    ERROR_CONFLICT, ERROR_INVALID_PARAMETER, ERROR_METHOD_NOT_FOUND, Event, METHOD_ATTACH,
    METHOD_EVENTS, METHOD_EXEC_CREATE, METHOD_EXEC_INSPECT, METHOD_EXEC_KILL, METHOD_EXEC_RESIZE,
    METHOD_EXEC_START, METHOD_KILL, METHOD_SET_CLOCK, METHOD_SHUTDOWN, ShutdownParams,
    SignalParams, Workload,
};
use clankerd_proto::varlink::{self, Call, Reply};
use serde_json::Value;

mod exec;
mod user;
mod volume;

const LISTEN_FD: i32 = 3;
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// How long to keep serving after the workload exits so subscribers see `exited`.
const LINGER: Duration = Duration::from_millis(300);
const DRAIN_LIMIT: Duration = Duration::from_secs(5);

/// Set once guestd booted a root disk: it is then PID 1 of a VM and may set the clock.
static BOOTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

struct Shared {
    inner: Mutex<Inner>,
    cv: Condvar,
    execs: exec::Registry,
    attach: Arc<attach::Attach>,
}

#[derive(Default)]
struct Inner {
    exit_code: Option<i32>,
    streams: usize,
    /// Process id of the workload while it runs.
    workload_pid: Option<i32>,
}

fn main() {
    let mut config = None;
    let mut exit_file = None;
    let mut host_socket = None;
    let mut host_vsock_port = None;
    let mut vsock_port = None;
    let mut loopback = None;
    let mut populate = false;
    let mut boot_dir = None;
    let mut lenient = false;
    let mut boot_root = false;
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
            "--populate" => populate = true,
            "--boot-dir" => boot_dir = args.next().map(PathBuf::from),
            "--lenient-ownership" => lenient = true,
            "--boot" => boot_root = true,
            other => die(&format!("unknown argument {other}")),
        }
    }
    let workload: Option<Workload> = if populate {
        None
    } else {
        let config = config.unwrap_or_else(|| die("--config is required"));
        Some(
            std::fs::read(&config)
                .map_err(|e| e.to_string())
                .and_then(|b| serde_json::from_slice(&b).map_err(|e| e.to_string()))
                .unwrap_or_else(|e| die(&format!("reading {}: {e}", config.display()))),
        )
    };

    BOOTED.store(boot_root, std::sync::atomic::Ordering::Relaxed);
    if boot_root {
        // Mount the root disk and pivot into it; from here on `/` is the
        // image. The boot directory stays reachable at its guest mount point.
        if let Err(e) = boot::init(workload.as_ref().unwrap_or(&Workload::default())) {
            die(&e);
        }
    }
    let boot_dir = boot_dir.unwrap_or_else(|| {
        if boot_root {
            PathBuf::from(clankerd_proto::spawn::GUEST_BOOT_MOUNT)
        } else {
            std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(PathBuf::from))
                .unwrap_or_else(|| PathBuf::from("/"))
        }
    });
    let disk = Arc::new(rootdisk::Ctx {
        boot_dir,
        populate_mode: populate,
        lenient,
    });

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
        execs: exec::Registry::new(
            workload.as_ref().map(|w| w.env.clone()).unwrap_or_default(),
            workload
                .as_ref()
                .map(|w| w.working_dir.clone())
                .unwrap_or_default(),
            workload
                .as_ref()
                .map(|w| w.user.clone())
                .unwrap_or_default(),
        ),
        attach: Arc::default(),
    });
    if let Some(workload) = &workload {
        let child = spawn_workload(workload, &shared.attach);
        if let Ok(c) = &child {
            shared.inner.lock().unwrap().workload_pid = Some(c.id() as i32);
        }
        let shared = shared.clone();
        std::thread::spawn(move || supervise(child, shared, exit_file));
    }

    loop {
        let Some(conn) = listener.accept() else {
            continue;
        };
        let shared = shared.clone();
        let tunnels = tunnels.clone();
        let disk = disk.clone();
        std::thread::spawn(move || serve(conn, shared, tunnels, disk));
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
fn spawn_workload(w: &Workload, attach: &Arc<attach::Attach>) -> Result<std::process::Child, i32> {
    let Some((prog, rest)) = w.argv.split_first() else {
        eprintln!("clankerd-guestd: no command to run");
        return Err(127);
    };
    let user = user::lookup(&w.user).map_err(|e| {
        eprintln!("clankerd-guestd: {e}");
        126
    })?;
    let mut cmd = Command::new(prog);
    cmd.args(rest).stdin(Stdio::null()).env_clear();
    cmd.env("PATH", DEFAULT_PATH);
    // Like Docker: HOME is the user's home unless the image sets it.
    if let Some(home) = user
        .as_ref()
        .and_then(|u| u.home.clone())
        .or_else(user::root_home)
    {
        cmd.env("HOME", home);
    }
    for kv in &w.env {
        if let Some((k, v)) = kv.split_once('=') {
            cmd.env(k, v);
        }
    }
    if !w.working_dir.is_empty() {
        cmd.current_dir(&w.working_dir);
    }
    if let Some(user) = &user {
        user::drop_privileges(&mut cmd, user);
    }
    let spawned = if attach::Attach::wanted(w) {
        attach.spawn(&mut cmd, w)
    } else {
        power::spawn_owned(&mut cmd)
    };
    spawned.map_err(|e| {
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
    let (code, signal) = match child {
        Ok(mut c) => {
            let status = c.wait();
            power::release(c.id());
            match status {
                Ok(s) => (
                    s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
                    s.signal(),
                ),
                Err(_) => (255, None),
            }
        }
        Err(code) => (code, None),
    };
    shared.attach.finish(clankerd_proto::guest::ExecStatus {
        exit_code: code,
        signal,
    });
    if let Some(exit_file) = exit_file {
        let tmp = exit_file.with_extension("tmp");
        if std::fs::write(&tmp, code.to_string()).is_ok() {
            let _ = std::fs::rename(&tmp, &exit_file);
        }
    }
    {
        let mut g = shared.inner.lock().unwrap();
        g.exit_code = Some(code);
        g.workload_pid = None;
    }
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
    power::exit_machine()
}

fn serve(
    conn: UnixStream,
    shared: Arc<Shared>,
    tunnels: Arc<tunnel::Tunnels>,
    disk: Arc<rootdisk::Ctx>,
) {
    let Ok(mut out) = conn.try_clone() else {
        return;
    };
    let mut input = BufReader::new(conn);
    let Ok(Some(call)) = varlink::read::<Call, _>(&mut input) else {
        return;
    };
    if rootdisk::handle(&call, &mut input, &mut out, &disk) {
        return;
    }
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
        METHOD_ATTACH => attach(input, out, call.parameters, &shared),
        METHOD_SHUTDOWN | METHOD_KILL => {
            let result = signal_workload(&call, &shared);
            let _ = reply(&mut out, result);
        }
        METHOD_SET_CLOCK => {
            let result = params::<clankerd_proto::guest::Clock>(&call).and_then(|clock| {
                // Only a booted guest owns its clock; a local stand-in must
                // never touch the host's.
                if BOOTED.load(std::sync::atomic::Ordering::Relaxed) {
                    boot::set_clock(&clock)
                        .map_err(|e| (ERROR_CONFLICT, format!("setting the clock: {e}")))?;
                }
                Ok(serde_json::json!({}))
            });
            let _ = reply(&mut out, result);
        }
        METHOD_EXEC_CREATE | METHOD_EXEC_RESIZE | METHOD_EXEC_KILL | METHOD_EXEC_INSPECT => {
            let result = exec_call(&call, &shared.execs);
            let _ = reply(&mut out, result);
        }
        _ => {
            let _ = reply(&mut out, Err((ERROR_METHOD_NOT_FOUND, String::new())));
        }
    }
}

/// `Shutdown` (the stop signal, SIGTERM by default) and `Kill` (any signal) act on the workload only.
fn signal_workload(call: &Call, shared: &Shared) -> Result<Value, exec::Failure> {
    let signal = if call.method == METHOD_SHUTDOWN {
        // Parameters are optional: a bare Shutdown is SIGTERM.
        serde_json::from_value::<ShutdownParams>(call.parameters.clone())
            .map_or(libc::SIGTERM, |p| p.signal)
    } else {
        params::<SignalParams>(call)?.signal
    };
    let Some(pid) = shared.inner.lock().unwrap().workload_pid else {
        return Err((ERROR_CONFLICT, "the workload is not running".into()));
    };
    // SAFETY: plain signal delivery to the workload we spawned and have not reaped.
    if unsafe { libc::kill(pid, signal) } < 0 {
        return Err((
            ERROR_CONFLICT,
            format!(
                "signalling the workload: {}",
                std::io::Error::last_os_error()
            ),
        ));
    }
    Ok(serde_json::json!({}))
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

/// Upgrades the connection and attaches it to the workload's terminal or stdio.
fn attach(input: BufReader<UnixStream>, mut out: UnixStream, p: Value, shared: &Arc<Shared>) {
    let params = match serde_json::from_value::<clankerd_proto::guest::AttachParams>(p) {
        Ok(p) => p,
        Err(e) => {
            let _ = reply(&mut out, Err((ERROR_INVALID_PARAMETER, e.to_string())));
            return;
        }
    };
    shared.inner.lock().unwrap().streams += 1;
    let signal = |sig: i32| {
        if let Some(pid) = shared.inner.lock().unwrap().workload_pid {
            // SAFETY: plain signal delivery to the workload we spawned and have not reaped.
            unsafe { libc::kill(pid, sig) };
        }
    };
    shared.attach.run(params, input, out, signal, &reply);
    end_stream(shared);
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
