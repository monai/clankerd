//! Tunnels: byte streams between a host endpoint and a guest endpoint.
//!
//! A tunnel is its own connection (vsock in production, a unix socket in the
//! stand-in). The opener sends one varlink call naming the target, waits for the
//! reply, and from then on both sides speak minimal frames: one type byte, a
//! big-endian `u32` length, the payload. `DATA` carries bytes, `EOF` is a
//! half-close (the sender will write no more but keeps reading).
//!
//! Directions:
//! * host -> guest: [`METHOD_CONNECT`] on the guest's varlink endpoint
//!   (published ports);
//! * guest -> host: [`METHOD_HOST_CONNECT`] on the host's endpoint
//!   (host-gateway ports, socket bindings).
//!
//! Guest listeners are managed with [`METHOD_LISTEN`] / [`METHOD_UNLISTEN`].

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const METHOD_CONNECT: &str = "io.clankerd.Guest.Connect";
pub const METHOD_LISTEN: &str = "io.clankerd.Guest.Listen";
pub const METHOD_UNLISTEN: &str = "io.clankerd.Guest.Unlisten";
pub const METHOD_HOST_CONNECT: &str = "io.clankerd.Host.Connect";

pub const ERROR_CONNECT_FAILED: &str = "io.clankerd.ConnectFailed";
pub const ERROR_NOT_ALLOWED: &str = "io.clankerd.NotAllowed";
pub const ERROR_LISTEN_FAILED: &str = "io.clankerd.ListenFailed";
pub const ERROR_NO_SUCH_LISTENER: &str = "io.clankerd.NoSuchListener";

/// Where a tunnel ends. `Tcp` is always on the loopback of the side that dials.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Target {
    Tcp { port: u16 },
    Unix { path: PathBuf },
}

/// Parameters of [`METHOD_CONNECT`] and [`METHOD_HOST_CONNECT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connect {
    pub target: Target,
}

/// Parameters of [`METHOD_LISTEN`]: the guest listens on `listen` (TCP on the
/// guest's loopback, or a unix path) and tunnels every connection to `target`
/// on the host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listen {
    pub listen: Target,
    pub target: Target,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listening {
    pub id: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unlisten {
    pub id: u64,
}

const FRAME_DATA: u8 = 0;
const FRAME_EOF: u8 = 1;
/// Largest payload of one `DATA` frame.
pub const MAX_PAYLOAD: usize = 64 * 1024;

/// One frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Frame {
    Data(Vec<u8>),
    Eof,
}

pub fn write_frame<W: Write>(w: &mut W, frame: &Frame) -> io::Result<()> {
    let (kind, payload): (u8, &[u8]) = match frame {
        Frame::Data(d) => (FRAME_DATA, d),
        Frame::Eof => (FRAME_EOF, &[]),
    };
    let mut buf = Vec::with_capacity(5 + payload.len());
    buf.push(kind);
    buf.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(payload);
    w.write_all(&buf)?;
    w.flush()
}

/// Reads one frame; `Ok(None)` when the stream ended without an `EOF` frame.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 5];
    match r.read_exact(&mut head) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "tunnel frame too large",
        ));
    }
    let mut payload = vec![0; len];
    r.read_exact(&mut payload)?;
    match head[0] {
        FRAME_DATA => Ok(Some(Frame::Data(payload))),
        FRAME_EOF => Ok(Some(Frame::Eof)),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown tunnel frame type {other}"),
        )),
    }
}

/// A connected byte stream that can be split and half-closed.
pub trait Duplex: Read + Write + Send + Sized + 'static {
    fn try_clone(&self) -> io::Result<Self>;
    fn shutdown(&self, how: Shutdown) -> io::Result<()>;
}

impl Duplex for TcpStream {
    fn try_clone(&self) -> io::Result<Self> {
        TcpStream::try_clone(self)
    }
    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        TcpStream::shutdown(self, how)
    }
}

impl Duplex for UnixStream {
    fn try_clone(&self) -> io::Result<Self> {
        UnixStream::try_clone(self)
    }
    fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        UnixStream::shutdown(self, how)
    }
}

/// Connects `sock` to a framed stream until both directions finished.
///
/// `frames_in` / `frames_out` are the two halves of the framed connection
/// (`frames_in` may hold bytes already buffered while reading the varlink reply).
/// Half-close is preserved both ways: an `EOF` frame becomes `shutdown(Write)` on
/// `sock`, and EOF from `sock` becomes an `EOF` frame.
pub fn pump<R, W, S>(mut frames_in: R, mut frames_out: W, sock: S)
where
    R: Read + Send + 'static,
    W: Write + Send + 'static,
    S: Duplex,
{
    let Ok(mut sock_in) = sock.try_clone() else {
        return;
    };
    let mut sock_out = sock;

    let to_peer = std::thread::spawn(move || {
        let mut buf = vec![0u8; MAX_PAYLOAD];
        loop {
            match sock_in.read(&mut buf) {
                Ok(0) => {
                    let _ = write_frame(&mut frames_out, &Frame::Eof);
                    break;
                }
                Ok(n) => {
                    if write_frame(&mut frames_out, &Frame::Data(buf[..n].to_vec())).is_err() {
                        let _ = sock_in.shutdown(Shutdown::Both);
                        break;
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    let _ = write_frame(&mut frames_out, &Frame::Eof);
                    break;
                }
            }
        }
    });

    loop {
        match read_frame(&mut frames_in) {
            Ok(Some(Frame::Data(d))) => {
                if sock_out.write_all(&d).is_err() {
                    // Nobody reads on that side any more: stop both directions.
                    let _ = sock_out.shutdown(Shutdown::Both);
                    break;
                }
            }
            Ok(Some(Frame::Eof)) => {
                let _ = sock_out.shutdown(Shutdown::Write);
                break;
            }
            Ok(None) | Err(_) => {
                // The framed side vanished: nothing more will arrive either way.
                let _ = sock_out.shutdown(Shutdown::Both);
                break;
            }
        }
    }
    let _ = to_peer.join();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &Frame::Data(b"abc".to_vec())).unwrap();
        write_frame(&mut wire, &Frame::Eof).unwrap();
        assert_eq!(wire, [0, 0, 0, 0, 3, b'a', b'b', b'c', 1, 0, 0, 0, 0]);
        let mut r = wire.as_slice();
        assert_eq!(
            read_frame(&mut r).unwrap(),
            Some(Frame::Data(b"abc".to_vec()))
        );
        assert_eq!(read_frame(&mut r).unwrap(), Some(Frame::Eof));
        assert_eq!(read_frame(&mut r).unwrap(), None);
    }

    #[test]
    fn oversized_and_unknown_frames_are_rejected() {
        let mut r: &[u8] = &[0, 0xff, 0xff, 0xff, 0xff];
        assert!(read_frame(&mut r).is_err());
        let mut r: &[u8] = &[9, 0, 0, 0, 0];
        assert!(read_frame(&mut r).is_err());
    }

    #[test]
    fn targets_use_the_documented_shape() {
        assert_eq!(
            serde_json::to_string(&Target::Tcp { port: 80 }).unwrap(),
            r#"{"kind":"tcp","port":80}"#
        );
    }
}
