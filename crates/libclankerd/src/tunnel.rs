//! Host side of tunnels: loopback listeners that carry connections into the
//! guest (published ports), and the endpoint the guest dials to reach host
//! loopback ports and host unix sockets.
//!
//! Everything here is independent of the network backend: it only needs the
//! guest's varlink endpoint and the host endpoint (vsock in production, unix
//! sockets in the stand-in VMM). Every listener is closed synchronously when its
//! handle is dropped.

use std::io::BufReader;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use clankerd_proto::tunnel::{
    Connect, ERROR_NOT_ALLOWED, Listen, Listening, METHOD_CONNECT, METHOD_HOST_CONNECT,
    METHOD_LISTEN, METHOD_UNLISTEN, Target, Unlisten, pump,
};
use clankerd_proto::varlink::{self, Call, Reply};
use serde_json::Value;

use crate::config::{HostConfig, PortBinding};
use crate::error::{Error, Result};

const POLL: Duration = Duration::from_millis(20);

/// Checks a binding before anything is bound: published ports are loopback only.
pub(crate) fn validate_port_binding(b: &PortBinding) -> Result<()> {
    if let Some(ip) = b.host_ip
        && !ip.is_loopback()
    {
        return Err(Error::invalid_parameter(format!(
            "cannot publish on {ip}: published ports are bound to loopback only"
        )));
    }
    if b.guest_port == 0 {
        return Err(Error::invalid_parameter("guest port must not be 0"));
    }
    Ok(())
}

pub(crate) fn validate_host_config(h: &HostConfig) -> Result<()> {
    for b in &h.port_bindings {
        validate_port_binding(b)?;
    }
    if h.host_gateway_ports.contains(&0) {
        return Err(Error::invalid_parameter("host-gateway port must not be 0"));
    }
    for s in &h.socket_bindings {
        if !s.guest_path.is_absolute() || !s.host_path.is_absolute() {
            return Err(Error::invalid_parameter(
                "socket binding paths must be absolute",
            ));
        }
    }
    Ok(())
}

/// A thread that accepts in a polling loop and can be stopped synchronously,
/// so the listener it owns is closed by the time `stop` returns.
struct Acceptor {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Acceptor {
    fn spawn(mut accept_once: impl FnMut() -> bool + Send + 'static) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if !accept_once() {
                    std::thread::sleep(POLL);
                }
            }
        });
        Acceptor {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Acceptor {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Opens a tunnel to `target` in the guest. Returns the framed connection's halves.
fn dial_guest(
    guest_socket: &Path,
    target: Target,
) -> std::io::Result<(BufReader<UnixStream>, UnixStream)> {
    let mut conn = UnixStream::connect(guest_socket)?;
    varlink::write(
        &mut conn,
        &Call {
            method: METHOD_CONNECT.into(),
            parameters: serde_json::to_value(Connect { target }).unwrap(),
            more: false,
            upgrade: true,
        },
    )?;
    let out = conn.try_clone()?;
    let mut input = BufReader::new(conn);
    match varlink::read::<Reply, _>(&mut input)? {
        Some(r) if r.error.is_none() => Ok((input, out)),
        Some(r) => Err(std::io::Error::other(r.error.unwrap_or_default())),
        None => Err(std::io::ErrorKind::UnexpectedEof.into()),
    }
}

/// A published port: a host listener whose connections go to a guest TCP port.
/// Dropping it closes the listener (established connections finish).
pub struct PublishedPort {
    addr: SocketAddr,
    _acceptor: Acceptor,
}

impl PublishedPort {
    pub(crate) fn bind(binding: &PortBinding, guest_socket: PathBuf) -> Result<Self> {
        validate_port_binding(binding)?;
        let ip = binding.host_ip.unwrap_or(IpAddr::V4(Ipv4Addr::LOCALHOST));
        let listener = TcpListener::bind(SocketAddr::new(ip, binding.host_port))
            .map_err(|e| Error::conflict(format!("cannot bind {ip}:{}: {e}", binding.host_port)))?;
        listener.set_nonblocking(true)?;
        let addr = listener.local_addr()?;
        let guest_port = binding.guest_port;
        let acceptor = Acceptor::spawn(move || match listener.accept() {
            Ok((sock, _)) => {
                let guest_socket = guest_socket.clone();
                std::thread::spawn(move || {
                    // BSD-derived systems let accepted sockets inherit non-blocking mode.
                    if sock.set_nonblocking(false).is_err() {
                        return;
                    }
                    if let Ok((input, out)) =
                        dial_guest(&guest_socket, Target::Tcp { port: guest_port })
                    {
                        pump(input, out, sock);
                    }
                });
                true
            }
            Err(_) => false,
        });
        Ok(PublishedPort {
            addr,
            _acceptor: acceptor,
        })
    }

    /// The host address the port is listening on (always loopback).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn host_port(&self) -> u16 {
        self.addr.port()
    }
}

/// Host endpoint the guest dials; only targets in `allowed` may be reached.
struct HostServer {
    path: PathBuf,
    _acceptor: Acceptor,
}

type Allowed = Arc<Mutex<Vec<Target>>>;

impl HostServer {
    fn start(path: PathBuf, allowed: Allowed) -> Result<Self> {
        let _ = std::fs::remove_file(&path);
        let listener = UnixListener::bind(&path)
            .map_err(|e| Error::system(format!("binding {}: {e}", path.display())))?;
        listener.set_nonblocking(true)?;
        let acceptor = Acceptor::spawn(move || match listener.accept() {
            Ok((conn, _)) => {
                let allowed = allowed.clone();
                std::thread::spawn(move || serve_guest(conn, allowed));
                true
            }
            Err(_) => false,
        });
        Ok(HostServer {
            path,
            _acceptor: acceptor,
        })
    }
}

impl Drop for HostServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn serve_guest(conn: UnixStream, allowed: Allowed) {
    if conn.set_nonblocking(false).is_err() {
        return;
    }
    let Ok(mut out) = conn.try_clone() else {
        return;
    };
    let mut input = BufReader::new(conn);
    let Ok(Some(call)) = varlink::read::<Call, _>(&mut input) else {
        return;
    };
    let target = (call.method == METHOD_HOST_CONNECT)
        .then(|| serde_json::from_value::<Connect>(call.parameters).ok())
        .flatten()
        .map(|c| c.target)
        .filter(|t| allowed.lock().unwrap().contains(t));
    let fail = |out: &mut UnixStream, error: &str| {
        let _ = varlink::write(
            out,
            &Reply {
                parameters: Value::Null,
                continues: false,
                error: Some(error.into()),
            },
        );
    };
    let Some(target) = target else {
        return fail(&mut out, ERROR_NOT_ALLOWED);
    };
    let ok = Reply {
        parameters: serde_json::json!({}),
        continues: false,
        error: None,
    };
    match target {
        Target::Tcp { port } => {
            match TcpStream::connect_timeout(
                &SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port),
                Duration::from_secs(5),
            ) {
                Ok(sock) if varlink::write(&mut out, &ok).is_ok() => pump(input, out, sock),
                _ => fail(&mut out, clankerd_proto::tunnel::ERROR_CONNECT_FAILED),
            }
        }
        Target::Unix { path } => match UnixStream::connect(path) {
            Ok(sock) if varlink::write(&mut out, &ok).is_ok() => pump(input, out, sock),
            _ => fail(&mut out, clankerd_proto::tunnel::ERROR_CONNECT_FAILED),
        },
    }
}

/// Plain varlink call on the guest's endpoint; returns the reply parameters.
fn guest_call(socket: &Path, method: &str, parameters: Value) -> Result<Value> {
    let mut conn = UnixStream::connect(socket)
        .map_err(|e| Error::unavailable(format!("guest is unreachable: {e}")))?;
    let io = |e: std::io::Error| Error::unavailable(format!("guest call failed: {e}"));
    varlink::write(
        &mut conn,
        &Call {
            method: method.into(),
            parameters,
            more: false,
            upgrade: false,
        },
    )
    .map_err(io)?;
    let reply = varlink::read::<Reply, _>(&mut BufReader::new(conn))
        .map_err(io)?
        .ok_or_else(|| Error::unavailable("guest closed the connection"))?;
    match reply.error {
        None => Ok(reply.parameters),
        Some(e) => {
            let detail = reply.parameters["message"].as_str().unwrap_or_default();
            Err(Error::unavailable(format!("{e}: {detail}")))
        }
    }
}

/// A listener inside the guest (host-gateway port or socket binding).
/// Dropping it closes the guest listener and revokes the host target.
pub struct GuestBinding {
    guest_socket: PathBuf,
    id: u64,
    target: Target,
    allowed: Allowed,
}

impl GuestBinding {
    /// Keeps the binding for the life of the guest instead of closing it on drop.
    pub(crate) fn detach(self) {
        let _ = std::mem::ManuallyDrop::new(self);
    }
}

impl Drop for GuestBinding {
    fn drop(&mut self) {
        let _ = guest_call(
            &self.guest_socket,
            METHOD_UNLISTEN,
            serde_json::to_value(Unlisten { id: self.id }).unwrap(),
        );
        revoke(&self.allowed, &self.target);
    }
}

fn revoke(allowed: &Allowed, target: &Target) {
    let mut a = allowed.lock().unwrap();
    if let Some(i) = a.iter().position(|t| t == target) {
        a.remove(i);
    }
}

/// Tunnel resources of one running machine.
pub(crate) struct MachineTunnels {
    guest_socket: PathBuf,
    allowed: Allowed,
    _server: HostServer,
    /// Ports published by the machine's configuration; closed with the machine.
    configured: Mutex<Vec<PublishedPort>>,
}

impl MachineTunnels {
    pub fn start(guest_socket: PathBuf, host_socket: PathBuf) -> Result<Arc<Self>> {
        let allowed = Allowed::default();
        let server = HostServer::start(host_socket, allowed.clone())?;
        Ok(Arc::new(MachineTunnels {
            guest_socket,
            allowed,
            _server: server,
            configured: Mutex::default(),
        }))
    }

    /// Publishes the configured ports (host side only; they live as long as `self`).
    pub fn publish_configured(&self, host: &HostConfig) -> Result<()> {
        let mut configured = self.configured.lock().unwrap();
        for b in &host.port_bindings {
            configured.push(self.publish(b)?);
        }
        Ok(())
    }

    /// Sets up the configured guest listeners (host-gateway ports, socket bindings).
    /// Only done on a fresh boot: after a reattach the guest still has them.
    pub fn listen_configured(&self, host: &HostConfig) -> Result<()> {
        // These live as long as the guest, so the handles are not kept.
        for port in &host.host_gateway_ports {
            self.expose_host_port(*port)?.detach();
        }
        for s in &host.socket_bindings {
            self.bind_socket(&s.guest_path, &s.host_path)?.detach();
        }
        Ok(())
    }

    pub fn publish(&self, binding: &PortBinding) -> Result<PublishedPort> {
        PublishedPort::bind(binding, self.guest_socket.clone())
    }

    pub fn expose_host_port(&self, port: u16) -> Result<GuestBinding> {
        self.listen(Target::Tcp { port }, Target::Tcp { port })
    }

    pub fn bind_socket(&self, guest_path: &Path, host_path: &Path) -> Result<GuestBinding> {
        self.listen(
            Target::Unix {
                path: guest_path.to_owned(),
            },
            Target::Unix {
                path: host_path.to_owned(),
            },
        )
    }

    fn listen(&self, listen: Target, target: Target) -> Result<GuestBinding> {
        self.allowed.lock().unwrap().push(target.clone());
        let reply = guest_call(
            &self.guest_socket,
            METHOD_LISTEN,
            serde_json::to_value(Listen {
                listen,
                target: target.clone(),
            })
            .unwrap(),
        );
        match reply.and_then(|v| {
            serde_json::from_value::<Listening>(v).map_err(|e| Error::system(e.to_string()))
        }) {
            Ok(Listening { id }) => Ok(GuestBinding {
                guest_socket: self.guest_socket.clone(),
                id,
                target,
                allowed: self.allowed.clone(),
            }),
            Err(e) => {
                revoke(&self.allowed, &target);
                Err(e)
            }
        }
    }
}
