//! Forwarding owned by the VM helper, with a host-only control socket for library callers.

use std::collections::HashMap;
use std::io::BufReader;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clankerd_proto::guest::Event;
use clankerd_proto::spawn::ForwardingConfig;
use clankerd_proto::tunnel::{Listen, Target};
use clankerd_proto::varlink::{self, Call, Reply};
use serde_json::{Value, json};

use crate::guest::{EventStream, Next};
use crate::tunnel::{Acceptor, GuestBinding, MachineTunnels, PublishedPort, validate_host_config};
use crate::{Error, ErrorKind, HostConfig, PortBinding, Result};

enum Binding {
    Published(PublishedPort),
    Guest(GuestBinding),
}

struct Control {
    tunnels: Arc<MachineTunnels>,
    bindings: Mutex<HashMap<u64, Binding>>,
    next: AtomicU64,
    ready: Mutex<Option<std::result::Result<(), String>>>,
}

/// Kept alive by clankerd-vmspawn for the entire VM lifetime.
pub struct ForwardingServer {
    _acceptor: Acceptor,
    path: PathBuf,
}

impl ForwardingServer {
    pub fn start(config: &ForwardingConfig, guest: PathBuf, host: PathBuf) -> Result<Self> {
        let host_config = HostConfig {
            port_bindings: config.port_bindings.clone(),
            host_gateway_ports: config.host_gateway_ports.clone(),
            socket_bindings: config.socket_bindings.clone(),
            ..Default::default()
        };
        validate_host_config(&host_config)?;
        let tunnels = MachineTunnels::start(guest.clone(), host)?;
        tunnels.publish_configured(&host_config)?;
        match std::fs::remove_file(&config.control_socket) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let listener = UnixListener::bind(&config.control_socket)?;
        std::fs::set_permissions(
            &config.control_socket,
            std::fs::Permissions::from_mode(0o600),
        )?;
        listener.set_nonblocking(true)?;
        let control = Arc::new(Control {
            tunnels,
            bindings: Mutex::default(),
            next: AtomicU64::new(1),
            ready: Mutex::new(None),
        });
        let initialization = control.clone();
        std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(120);
            let result = loop {
                if let Ok(mut stream) = EventStream::connect(&guest)
                    && matches!(
                        stream.next(Some(Instant::now() + Duration::from_millis(250))),
                        Next::Event(Event::Ready)
                    )
                {
                    drop(stream);
                    break initialization
                        .tunnels
                        .listen_configured(&host_config)
                        .map_err(|e| e.to_string());
                }
                if Instant::now() >= deadline {
                    break Err("guest did not become ready for forwarding".to_owned());
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            *initialization.ready.lock().unwrap() = Some(result);
        });
        let acceptor = Acceptor::spawn(move || match listener.accept() {
            Ok((connection, _)) => {
                let control = control.clone();
                std::thread::spawn(move || serve(connection, &control));
                true
            }
            Err(_) => false,
        });
        Ok(Self {
            _acceptor: acceptor,
            path: config.control_socket.clone(),
        })
    }
}

impl Drop for ForwardingServer {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn serve(mut connection: UnixStream, control: &Control) {
    if connection.set_nonblocking(false).is_err() {
        return;
    }
    let _ = connection.set_read_timeout(Some(Duration::from_secs(10)));
    let Ok(Some(request)) = varlink::read::<Call, _>(&mut BufReader::new(&mut connection)) else {
        return;
    };
    let result = dispatch(request, control);
    let reply = match result {
        Ok(parameters) => Reply {
            parameters,
            continues: false,
            error: None,
        },
        Err(error) => Reply {
            parameters: json!({"message": error.message()}),
            continues: false,
            error: Some(format!("io.clankerd.Forwarding.{:?}", error.kind())),
        },
    };
    let _ = varlink::write(&mut connection, &reply);
}

fn dispatch(request: Call, control: &Control) -> Result<Value> {
    match request
        .method
        .strip_prefix("io.clankerd.Forwarding.")
        .unwrap_or_default()
    {
        "Ready" => match &*control.ready.lock().unwrap() {
            None => Ok(json!(false)),
            Some(Ok(())) => Ok(json!(true)),
            Some(Err(message)) => Err(Error::unavailable(message.clone())),
        },
        "Publish" => {
            let binding: PortBinding = serde_json::from_value(request.parameters)?;
            let port = control.tunnels.publish(&binding)?;
            let addr: SocketAddr = port.local_addr();
            let id = control.next.fetch_add(1, Ordering::Relaxed);
            control
                .bindings
                .lock()
                .unwrap()
                .insert(id, Binding::Published(port));
            Ok(json!([id, addr]))
        }
        "Listen" => {
            let listen: Listen = serde_json::from_value(request.parameters)?;
            let binding = match (listen.listen, listen.target) {
                (Target::Tcp { port }, Target::Tcp { port: target })
                    if port == target && port != 0 =>
                {
                    control.tunnels.expose_host_port(port)?
                }
                (Target::Unix { path: guest }, Target::Unix { path: host })
                    if guest.is_absolute() && host.is_absolute() =>
                {
                    control.tunnels.bind_socket(&guest, &host)?
                }
                _ => return Err(Error::invalid_parameter("invalid forwarding listener")),
            };
            let id = control.next.fetch_add(1, Ordering::Relaxed);
            control
                .bindings
                .lock()
                .unwrap()
                .insert(id, Binding::Guest(binding));
            Ok(json!(id))
        }
        "Remove" => {
            let id: u64 = serde_json::from_value(request.parameters["id"].clone())?;
            let binding = control.bindings.lock().unwrap().remove(&id);
            match binding {
                Some(Binding::Published(port)) => drop(port),
                Some(Binding::Guest(binding)) => drop(binding),
                None => {}
            }
            Ok(json!({}))
        }
        _ => Err(Error::invalid_parameter("unknown forwarding method")),
    }
}

pub(crate) fn call(socket: &Path, method: &str, parameters: Value) -> Result<Value> {
    let mut connection = UnixStream::connect(socket)
        .map_err(|e| Error::unavailable(format!("forwarding helper {}: {e}", socket.display())))?;
    connection.set_read_timeout(Some(Duration::from_secs(10)))?;
    connection.set_write_timeout(Some(Duration::from_secs(10)))?;
    varlink::write(
        &mut connection,
        &Call {
            method: format!("io.clankerd.Forwarding.{method}"),
            parameters,
            more: false,
            upgrade: false,
        },
    )?;
    let reply: Reply = varlink::read(&mut BufReader::new(connection))?
        .ok_or_else(|| Error::unavailable("forwarding helper closed the connection"))?;
    if let Some(error) = reply.error {
        let kind = match error.rsplit('.').next() {
            Some("Conflict") => ErrorKind::Conflict,
            Some("InvalidParameter") => ErrorKind::InvalidParameter,
            Some("NotFound") => ErrorKind::NotFound,
            Some("System") => ErrorKind::System,
            _ => ErrorKind::Unavailable,
        };
        return Err(Error::new(
            kind,
            reply.parameters["message"].as_str().unwrap_or_default(),
        ));
    }
    Ok(reply.parameters)
}

pub(crate) fn wait_ready(socket: &Path) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if call(socket, "Ready", json!({}))?.as_bool() == Some(true) {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::unavailable(
                "timed out waiting for forwarding initialization",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
