//! clankerd-guestd: guest-side agent. In this tracer bullet it runs the workload
//! and serves `io.clankerd.Guest.Events` on a socket inherited via `LISTEN_FDS`
//! (a unix socket standing in for vsock). Real VM boot uses the packaged guest management environment.
//!
//! Usage: `clankerd-guestd --config WORKLOAD.json [--exit-file PATH]
//! [--vsock-port N] [--host-socket PATH | --host-vsock-port N]`
//!
//! The listening socket is either inherited via `LISTEN_FDS` (local-process
//! stand-in) or an `AF_VSOCK` listener on `--vsock-port` (real VM). The host
//! tunnel endpoint is a unix socket (`--host-socket`) or a vsock port on the
//! host (`--host-vsock-port`).
//!
//! The optional exit file receives the workload's exit code so the result
//! survives the death of whoever was subscribed. Inside a VM the boot directory
//! is read-only by convention, so the host side (clankerd-vmspawn) records it
//! from the `Exited` event instead.
//!
//! Population boot (`--populate`, no workload): serves only the root-disk
//! methods (format, unpack tar, populate disk) with the packaged e2fsprogs from
//! `--management-root` (required for local formatting); `--lenient-ownership` is a
//! development flag for runs without root.
//!
//! As PID 1 (booted by libkrun's init.krun) guestd outlives the OCI command.
//! An explicit Shutdown request ends the machine after delivering its result: it signals
//! and reaps every process, syncs, unmounts and powers off (`power`).
//!
//! With `--boot` guestd mounts the workload disk (`/dev/vda`), prepares a private
//! workload root, mounts /proc, /sys, /dev and cgroup2, delegates cgroup controllers,
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
mod workload_root;

use std::io::BufReader;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use clankerd_proto::guest::{
    ERROR_CONFLICT, ERROR_INVALID_PARAMETER, ERROR_METHOD_NOT_FOUND, Event, ExitAcknowledgment,
    ExitObserver, METHOD_ACKNOWLEDGE_EXIT, METHOD_ATTACH, METHOD_EVENTS, METHOD_EXEC_CREATE,
    METHOD_EXEC_INSPECT, METHOD_EXEC_KILL, METHOD_EXEC_RESIZE, METHOD_EXEC_START, METHOD_KILL,
    METHOD_SET_CLOCK, METHOD_SHUTDOWN, METHOD_STARTUP_COMPLETE, ShutdownParams, SignalParams,
    Workload,
};
use clankerd_proto::varlink::{self, Call, Reply};
use serde_json::Value;

mod exec;
mod user;
mod volume;

const LISTEN_FD: i32 = 3;
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
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
    exit_recorded: bool,
    shutdown_requested: bool,
    startup_complete: bool,
    expect_attach: bool,
    attach_acknowledged: bool,
}

fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("network-hook") => {
            if let Err(error) = network::hook::run() {
                die(&format!("network hook: {error}"));
            }
            return;
        }
        Some("namespace-keeper") => {
            if let Err(error) = workload_root::keeper() {
                die(&format!("namespace keeper: {error}"));
            }
            return;
        }
        _ => {}
    }
    let mut config = None;
    let mut exit_file = None;
    let mut host_socket = None;
    let mut host_vsock_port = None;
    let mut vsock_port = None;
    let mut populate = false;
    let mut management_root = None;
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
            "--populate" => populate = true,
            "--management-root" => management_root = args.next().map(PathBuf::from),
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
        // Keep PID1 in management and prepare the workload's private root.
        if let Err(e) = boot::init(workload.as_ref().unwrap_or(&Workload::default())) {
            die(&e);
        }
    }
    let management_root = management_root.or_else(|| boot_root.then(|| PathBuf::from("/")));
    let disk = Arc::new(rootdisk::Ctx {
        management_root,
        populate_mode: populate,
        populated: Default::default(),
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

    let tunnels = Arc::new(tunnel::Tunnels::new(host));

    // A local-process stand-in can observe explicitly configured loopback
    // destinations without configuring any host interface or resolver.
    if !boot_root
        && let Some(endpoints) = workload
            .as_ref()
            .and_then(|workload| workload.network.as_ref())
            .and_then(|network| network.probe_endpoints.clone())
    {
        network::probe::start(endpoints);
    }

    let shared = Arc::new(Shared {
        inner: Mutex::new(Inner {
            expect_attach: workload.as_ref().is_some_and(|w| w.expect_attach),
            ..Default::default()
        }),
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
    workload_root::configure(&mut cmd, &w.working_dir).map_err(|e| {
        eprintln!("workload namespace: {e}");
        126
    })?;
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

/// Records the command result, then waits for an explicit machine shutdown.
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

    {
        let mut g = shared.inner.lock().unwrap();
        g.exit_code = Some(code);
        g.workload_pid = None;
    }
    shared.attach.finish(clankerd_proto::guest::ExecStatus {
        exit_code: code,
        signal,
    });
    let mut exit_recorded = false;
    if let Some(exit_file) = exit_file {
        let tmp = exit_file.with_extension("tmp");
        match std::fs::write(&tmp, code.to_string())
            .and_then(|()| std::fs::rename(&tmp, &exit_file))
        {
            Ok(()) => exit_recorded = true,
            Err(e) => eprintln!("clankerd-guestd: recording workload exit: {e}"),
        }
    }
    {
        let mut g = shared.inner.lock().unwrap();
        g.exit_code = Some(code);
        g.workload_pid = None;
        g.exit_recorded |= exit_recorded;
    }
    shared.cv.notify_all();

    {
        let mut g = shared.inner.lock().unwrap();
        while !g.shutdown_requested {
            g = shared.cv.wait(g).unwrap();
        }
    }
    let deadline = Instant::now() + DRAIN_LIMIT;
    let acknowledged = {
        let mut g = shared.inner.lock().unwrap();
        while (!g.exit_recorded
            || !g.startup_complete
            || (g.expect_attach && !g.attach_acknowledged))
            && Instant::now() < deadline
        {
            g = shared
                .cv
                .wait_timeout(g, Duration::from_millis(50))
                .unwrap()
                .0;
        }
        (
            g.exit_recorded,
            g.startup_complete,
            !g.expect_attach || g.attach_acknowledged,
        )
    };

    if acknowledged != (true, true, true) {
        eprintln!("clankerd-guestd: shutdown acknowledgment timed out: {acknowledged:?}");
    }
    {
        let mut g = shared.inner.lock().unwrap();

        while g.streams > 0 && Instant::now() < deadline {
            g = shared
                .cv
                .wait_timeout(g, Duration::from_millis(50))
                .unwrap()
                .0;
        }
    }

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
        METHOD_STARTUP_COMPLETE => {
            let mut g = shared.inner.lock().unwrap();
            let _ = reply(&mut out, Ok(serde_json::json!({})));
            g.startup_complete = true;
            drop(g);
            shared.cv.notify_all();
        }
        METHOD_ACKNOWLEDGE_EXIT => match params::<ExitAcknowledgment>(&call) {
            Ok(ack) => {
                let mut g = shared.inner.lock().unwrap();
                if g.exit_code != Some(ack.exit_code) {
                    let _ = reply(
                        &mut out,
                        Err((
                            ERROR_CONFLICT,
                            "exit code is not available or does not match".into(),
                        )),
                    );
                } else {
                    let _ = reply(&mut out, Ok(serde_json::json!({})));
                    match ack.observer {
                        ExitObserver::Recorded => g.exit_recorded = true,
                        ExitObserver::Attached => g.attach_acknowledged = true,
                    }
                    drop(g);
                    shared.cv.notify_all();
                }
            }
            Err(error) => {
                let _ = reply(&mut out, Err(error));
            }
        },
        METHOD_EVENTS => {
            shared.inner.lock().unwrap().streams += 1;
            let _ = events(&mut out, &shared);
            end_stream(&shared);
        }
        METHOD_EXEC_START => exec_start(input, out, call.parameters, &shared),
        METHOD_ATTACH => attach(input, out, call.parameters, &shared),
        METHOD_SHUTDOWN | METHOD_KILL => {
            let result = signal_workload(&call, &shared);
            let shutdown = call.method == METHOD_SHUTDOWN && result.is_ok();
            let _ = reply(&mut out, result);
            if shutdown {
                let mut state = shared.inner.lock().unwrap();
                state.expect_attach = false;
                state.shutdown_requested = true;
            }
            shared.cv.notify_all();
        }
        clankerd_proto::guest::METHOD_NETWORK_STATUS => {
            let status = if BOOTED.load(std::sync::atomic::Ordering::Relaxed) {
                network::status("eth0")
            } else {
                let mut status = clankerd_proto::guest::NetworkStatus::default();
                network::probe::apply(&mut status);
                status
            };
            let _ = reply(&mut out, Ok(serde_json::to_value(status).unwrap()));
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

/// Shutdown requests machine teardown; other signals target the OCI command only.
fn signal_workload(call: &Call, shared: &Shared) -> Result<Value, exec::Failure> {
    let shutdown = call.method == METHOD_SHUTDOWN;
    let signal = if shutdown {
        if call.parameters.is_null() {
            libc::SIGTERM
        } else {
            params::<ShutdownParams>(call)?.signal
        }
    } else {
        params::<SignalParams>(call)?.signal
    };
    if let Some(pid) = shared.inner.lock().unwrap().workload_pid {
        // SAFETY: signal delivery to the command owned by this guest agent.
        if unsafe { libc::kill(pid, signal) } < 0 {
            let error = std::io::Error::last_os_error();
            if !shutdown || error.raw_os_error() != Some(libc::ESRCH) {
                return Err((ERROR_CONFLICT, format!("signalling the workload: {error}")));
            }
        }
    } else if !shutdown {
        return Err((ERROR_CONFLICT, "the workload is not running".into()));
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
    {
        let mut state = shared.inner.lock().unwrap();
        state.streams += 1;
        state.expect_attach = true;
    }
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
        // Events has no further client input. Observe its read half so a
        // detached observer releases both descriptors and its worker thread,
        // even when the workload has no events to wake the condition variable.
        let mut byte = 0u8;
        // SAFETY: out owns a live stream descriptor and byte is writable storage.
        let received = unsafe {
            libc::recv(
                out.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        if received == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "event observer disconnected",
            ));
        }
        if received < 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                return Err(error);
            }
        }
        g = shared
            .cv
            .wait_timeout(g, Duration::from_millis(100))
            .unwrap()
            .0;
    };
    drop(g);
    varlink::write(out, &reply(&Event::Exited { exit_code: code }, false))
}
