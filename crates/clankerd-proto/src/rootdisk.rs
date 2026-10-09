//! Root-disk methods of `io.clankerd.Guest`: formatting a blank disk and
//! unpacking the merged image tar onto it, done in the guest so that owners,
//! modes, xattrs, device nodes and mtimes get real Linux semantics.
//!
//! `PopulateDisk` sends tar bytes as stdin frames using [`crate::frame`], ending
//! with a stdin close frame. The socket stays open for the reply: libkrun's
//! vsock proxy resets the connection on a socket EOF.
//! `UnpackTar` uses raw tar bytes and a write half-close (local Unix sockets).
//! A population boot stays alive until the host receives that reply and sends
//! `FinishPopulation` on a new connection; this final call has no reply.

use serde::{Deserialize, Serialize};
use std::io::{self, Cursor, Read, Write};

use crate::frame::{self, CHANNEL_CLOSE, CHANNEL_STDIN, Frame, MAX_PAYLOAD};

/// Sends a tar stream followed by its end marker without shutting down the socket.
pub fn write_tar_stream<R: Read + ?Sized, W: Write>(input: &mut R, out: &mut W) -> io::Result<()> {
    let mut buffer = vec![0; MAX_PAYLOAD];
    loop {
        let n = match input.read(&mut buffer) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if n == 0 {
            return frame::write_frame(out, &Frame::close(CHANNEL_STDIN));
        }
        frame::write_chunks(out, CHANNEL_STDIN, &buffer[..n])?;
    }
}

/// Reads tar payloads until the explicit stdin close frame; socket EOF is an error.
pub struct TarStreamReader<R> {
    input: R,
    pending: Cursor<Vec<u8>>,
    done: bool,
}

impl<R: Read> TarStreamReader<R> {
    pub fn new(input: R) -> Self {
        Self {
            input,
            pending: Cursor::new(Vec::new()),
            done: false,
        }
    }
}

impl<R: Read> Read for TarStreamReader<R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        loop {
            let n = self.pending.read(buffer)?;
            if n > 0 || self.done {
                return Ok(n);
            }
            let next = frame::read_frame(&mut self.input)?.ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "tar stream ended without a close frame",
                )
            })?;
            match next.channel {
                CHANNEL_STDIN => self.pending = Cursor::new(next.payload),
                CHANNEL_CLOSE if next.payload == [CHANNEL_STDIN] => self.done = true,
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unexpected tar stream channel",
                    ));
                }
            }
        }
    }
}

/// `FormatExt4 { device, size?, label? }` -> `{}`. Runs the static mke2fs from the boot directory.
pub const METHOD_FORMAT_EXT4: &str = "io.clankerd.Guest.FormatExt4";
/// `UnpackTar { target }` + tar stream -> `UnpackSummary`. Unpacks into an existing directory.
pub const METHOD_UNPACK_TAR: &str = "io.clankerd.Guest.UnpackTar";
/// `PopulateDisk { device, size? }` + tar stream -> `UnpackSummary`.
/// Formats the disk, mounts it, unpacks the tar onto it, syncs and unmounts.
pub const METHOD_POPULATE_DISK: &str = "io.clankerd.Guest.PopulateDisk";
/// Acknowledges a successful population reply and powers off the population VM.
pub const METHOD_FINISH_POPULATION: &str = "io.clankerd.Guest.FinishPopulation";

/// The request was malformed (bad path, device missing).
pub const ERROR_INVALID_PARAMETER: &str = "io.clankerd.Guest.InvalidParameter";
/// The operation failed; the reply parameters carry `{"message": ...}`.
pub const ERROR_FAILED: &str = "io.clankerd.Guest.Failed";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FormatExt4 {
    /// Block device or regular file to format.
    pub device: String,
    /// Bytes; a regular file is extended to this size first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnpackTar {
    pub target: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PopulateDisk {
    pub device: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnpackSummary {
    pub entries: u64,
    /// Total size of regular file contents written.
    pub bytes: u64,
}

/// Parameters of an error reply.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub message: String,
}
