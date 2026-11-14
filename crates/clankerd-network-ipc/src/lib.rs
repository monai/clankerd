#[cfg(not(target_os = "macos"))]
use std::io;
use std::os::fd::OwnedFd;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub interface_id: String,
    pub operation_mode: String,
    pub start_address: Option<String>,
    pub end_address: Option<String>,
    pub subnet_mask: Option<String>,
    pub nat66_prefix: Option<String>,
    pub shared_interface: Option<String>,
    pub network_id: Option<String>,
    pub subnet: String,
    pub firewall: bool,
}

#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Create { key: String, settings: Settings },
    Attach { key: String, generation: String },
    Stop { key: String, generation: String },
    Status { key: String, generation: String },
    // A struct variant makes Serde enforce deny_unknown_fields here too.
    List {},
}

#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Response {
    pub generation: Option<String>,
    pub prefix: Option<String>,
    pub connected: bool,
    pub interfaces: Vec<String>,
}

#[cfg(target_os = "macos")]
mod codec;
#[cfg(target_os = "macos")]
mod ffi;
#[cfg(target_os = "macos")]
mod transport;
#[cfg(target_os = "macos")]
mod xpc;

#[derive(Debug, Clone, Copy)]
pub struct Peer {
    pub uid: u32,
    pub pid: i32,
    pub connection: u64,
}

/// A successful domain reply, owning any socket until XPC duplicates it.
pub struct Reply {
    pub response: Response,
    pub packet: Option<OwnedFd>,
}

/// Rust hooks; the IPC crate owns decoding, errors, ownership and C callbacks.
/// Hooks must not panic: unwinding across the C callback boundary aborts the process.
pub struct Server {
    pub request: fn(Request, Peer) -> Result<Reply, String>,
    pub disconnected: fn(u64),
    pub shutdown: fn(),
}

#[cfg(target_os = "macos")]
pub use transport::{Connection, serve};

#[cfg(not(target_os = "macos"))]
pub struct Connection;
#[cfg(not(target_os = "macos"))]
impl Connection {
    pub fn connect() -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "network daemon requires macOS",
        ))
    }
    pub fn request(&self, _: &Request) -> io::Result<Reply> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "network daemon requires macOS",
        ))
    }
}
