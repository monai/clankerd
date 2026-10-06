//! Guest side of tunnels: dials guest targets for the host (published ports) and
//! listens in the guest for connections to carry to the host (host-gateway
//! ports and socket bindings).

use std::collections::HashMap;
use std::io::BufReader;
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use clankerd_proto::tunnel::{
    Connect, Duplex, ERROR_CONNECT_FAILED, ERROR_LISTEN_FAILED, ERROR_NO_SUCH_LISTENER, Listen,
    Listening, METHOD_CONNECT, METHOD_HOST_CONNECT, METHOD_LISTEN, METHOD_UNLISTEN, Target,
    Unlisten, pump,
};
use clankerd_proto::varlink::{self, Call, Reply};
use serde_json::{Value, json};

const POLL: Duration = Duration::from_millis(20);

pub struct Tunnels {
    /// The guest's loopback address (127.0.0.1; the stand-in VMM shifts it).
    loopback: IpAddr,
    /// Endpoint of the host's tunnel server (vsock in production).
    host_socket: Option<PathBuf>,
    next_id: AtomicU64,
    listeners: Mutex<HashMap<u64, ListenerHandle>>,
}

struct ListenerHandle {
    stop: Arc<AtomicBool>,
    thread: std::thread::JoinHandle<()>,
}

pub fn handles(method: &str) -> bool {
    matches!(method, METHOD_CONNECT | METHOD_LISTEN | METHOD_UNLISTEN)
}

fn reply(out: &mut UnixStream, parameters: Value, error: Option<&str>) -> std::io::Result<()> {
    varlink::write(
        out,
        &Reply {
            parameters,
            continues: false,
            error: error.map(str::to_owned),
        },
    )
}

impl Tunnels {
    pub fn new(loopback: IpAddr, host_socket: Option<PathBuf>) -> Self {
        Tunnels {
            loopback,
            host_socket,
            next_id: AtomicU64::new(1),
            listeners: Mutex::default(),
        }
    }

    /// Serves one tunnel-related call on `conn`.
    pub fn handle(self: &Arc<Self>, call: Call, input: BufReader<UnixStream>, mut out: UnixStream) {
        let _ = match call.method.as_str() {
            METHOD_CONNECT => self.connect(call.parameters, input, out),
            METHOD_LISTEN => self.listen(call.parameters, &mut out),
            _ => self.unlisten(call.parameters, &mut out),
        };
    }

    fn connect(
        &self,
        params: Value,
        input: BufReader<UnixStream>,
        mut out: UnixStream,
    ) -> std::io::Result<()> {
        let Ok(Connect { target }) = serde_json::from_value(params) else {
            return reply(
                &mut out,
                Value::Null,
                Some("org.varlink.service.InvalidParameter"),
            );
        };
        match target {
            Target::Tcp { port } => {
                match TcpStream::connect_timeout(
                    &SocketAddr::new(self.loopback, port),
                    Duration::from_secs(5),
                ) {
                    Ok(sock) => {
                        reply(&mut out, json!({}), None)?;
                        pump(input, out, sock);
                        Ok(())
                    }
                    Err(e) => reply(
                        &mut out,
                        json!({"message": e.to_string()}),
                        Some(ERROR_CONNECT_FAILED),
                    ),
                }
            }
            Target::Unix { path } => match UnixStream::connect(&path) {
                Ok(sock) => {
                    reply(&mut out, json!({}), None)?;
                    pump(input, out, sock);
                    Ok(())
                }
                Err(e) => reply(
                    &mut out,
                    json!({"message": e.to_string()}),
                    Some(ERROR_CONNECT_FAILED),
                ),
            },
        }
    }

    fn listen(self: &Arc<Self>, params: Value, out: &mut UnixStream) -> std::io::Result<()> {
        let Ok(Listen { listen, target }) = serde_json::from_value::<Listen>(params) else {
            return reply(
                out,
                Value::Null,
                Some("org.varlink.service.InvalidParameter"),
            );
        };
        let Some(host_socket) = self.host_socket.clone() else {
            return reply(
                out,
                json!({"message": "no host endpoint configured"}),
                Some(ERROR_LISTEN_FAILED),
            );
        };
        let stop = Arc::new(AtomicBool::new(false));
        let thread = match self.start_listener(&listen, target, host_socket, stop.clone()) {
            Ok(t) => t,
            Err(e) => {
                return reply(
                    out,
                    json!({"message": e.to_string()}),
                    Some(ERROR_LISTEN_FAILED),
                );
            }
        };
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.listeners
            .lock()
            .unwrap()
            .insert(id, ListenerHandle { stop, thread });
        reply(out, serde_json::to_value(Listening { id }).unwrap(), None)
    }

    fn start_listener(
        &self,
        listen: &Target,
        target: Target,
        host_socket: PathBuf,
        stop: Arc<AtomicBool>,
    ) -> std::io::Result<std::thread::JoinHandle<()>> {
        match listen {
            Target::Tcp { port } => {
                let l = TcpListener::bind(SocketAddr::new(self.loopback, *port))?;
                l.set_nonblocking(true)?;
                Ok(std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match l.accept() {
                            Ok((sock, _)) => carry(sock, &host_socket, &target),
                            Err(_) => std::thread::sleep(POLL),
                        }
                    }
                }))
            }
            Target::Unix { path } => {
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                let _ = std::fs::remove_file(path);
                let l = UnixListener::bind(path)?;
                l.set_nonblocking(true)?;
                let path = path.clone();
                Ok(std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        match l.accept() {
                            Ok((sock, _)) => carry(sock, &host_socket, &target),
                            Err(_) => std::thread::sleep(POLL),
                        }
                    }
                    drop(l);
                    let _ = std::fs::remove_file(path);
                }))
            }
        }
    }

    fn unlisten(&self, params: Value, out: &mut UnixStream) -> std::io::Result<()> {
        let Ok(Unlisten { id }) = serde_json::from_value(params) else {
            return reply(
                out,
                Value::Null,
                Some("org.varlink.service.InvalidParameter"),
            );
        };
        let handle = self.listeners.lock().unwrap().remove(&id);
        match handle {
            Some(h) => {
                h.stop.store(true, Ordering::Relaxed);
                // Return only once the listener is really closed.
                let _ = h.thread.join();
                reply(out, json!({}), None)
            }
            None => reply(out, Value::Null, Some(ERROR_NO_SUCH_LISTENER)),
        }
    }
}

/// Carries one guest connection to `target` on the host.
fn carry<S: Duplex>(sock: S, host_socket: &PathBuf, target: &Target) {
    let host_socket = host_socket.clone();
    let target = target.clone();
    std::thread::spawn(move || {
        let Ok(mut conn) = UnixStream::connect(&host_socket) else {
            return;
        };
        let call = Call {
            method: METHOD_HOST_CONNECT.into(),
            parameters: serde_json::to_value(Connect { target }).unwrap(),
            more: false,
        };
        if varlink::write(&mut conn, &call).is_err() {
            return;
        }
        let Ok(out) = conn.try_clone() else { return };
        let mut input = BufReader::new(conn);
        match varlink::read::<Reply, _>(&mut input) {
            Ok(Some(r)) if r.error.is_none() => pump(input, out, sock),
            _ => {}
        }
    });
}
