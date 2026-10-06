//! Frame codec for raw streams that follow a varlink `upgrade` (exec, tunnels).
//!
//! A frame is `[u8 channel][u32 big-endian length][payload]`. Channel numbers
//! follow Kubernetes remotecommand v5 where one exists, plus a signal channel.
//! Tunnels reuse the codec with their own channel meanings.

use std::io::{self, Read, Write};

pub const CHANNEL_STDIN: u8 = 0;
pub const CHANNEL_STDOUT: u8 = 1;
pub const CHANNEL_STDERR: u8 = 2;
/// Terminal status of the stream (JSON, see `guest::ExecStatus`).
pub const CHANNEL_STATUS: u8 = 3;
/// Terminal resize (JSON, see `guest::ResizeParams`).
pub const CHANNEL_RESIZE: u8 = 4;
/// Signal to deliver (JSON, see `guest::SignalParams`).
pub const CHANNEL_SIGNAL: u8 = 5;
/// Half-close: the one-byte payload names the channel being closed.
pub const CHANNEL_CLOSE: u8 = 255;

/// Upper bound on one payload; larger writes are split by [`write_chunks`].
pub const MAX_PAYLOAD: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub channel: u8,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(channel: u8, payload: impl Into<Vec<u8>>) -> Self {
        Frame {
            channel,
            payload: payload.into(),
        }
    }

    /// A close frame for `channel`.
    pub fn close(channel: u8) -> Self {
        Frame::new(CHANNEL_CLOSE, [channel])
    }
}

/// Writes one frame (no flush; unix sockets are unbuffered).
pub fn write_frame<W: Write>(w: &mut W, frame: &Frame) -> io::Result<()> {
    if frame.payload.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "frame payload too large",
        ));
    }
    let mut buf = Vec::with_capacity(5 + frame.payload.len());
    buf.push(frame.channel);
    buf.extend_from_slice(&(frame.payload.len() as u32).to_be_bytes());
    buf.extend_from_slice(&frame.payload);
    w.write_all(&buf)
}

/// Writes `data` on `channel`, split into frames of at most [`MAX_PAYLOAD`].
pub fn write_chunks<W: Write>(w: &mut W, channel: u8, data: &[u8]) -> io::Result<()> {
    for chunk in data.chunks(MAX_PAYLOAD) {
        write_frame(w, &Frame::new(channel, chunk))?;
    }
    Ok(())
}

/// Reads one frame; `Ok(None)` on a clean end of stream between frames.
pub fn read_frame<R: Read>(r: &mut R) -> io::Result<Option<Frame>> {
    let mut head = [0u8; 5];
    let mut got = 0;
    while got < head.len() {
        match r.read(&mut head[got..]) {
            Ok(0) if got == 0 => return Ok(None),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_be_bytes([head[1], head[2], head[3], head[4]]) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame payload too large",
        ));
    }
    let mut payload = vec![0; len];
    r.read_exact(&mut payload)?;
    Ok(Some(Frame {
        channel: head[0],
        payload,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_with_documented_layout() {
        let mut wire = Vec::new();
        write_frame(&mut wire, &Frame::new(CHANNEL_STDOUT, b"hi".to_vec())).unwrap();
        assert_eq!(wire, [1, 0, 0, 0, 2, b'h', b'i']);
        write_frame(&mut wire, &Frame::close(CHANNEL_STDIN)).unwrap();
        let mut r = wire.as_slice();
        assert_eq!(
            read_frame(&mut r).unwrap().unwrap(),
            Frame::new(1, b"hi".to_vec())
        );
        assert_eq!(read_frame(&mut r).unwrap().unwrap(), Frame::close(0));
        assert_eq!(read_frame(&mut r).unwrap(), None);
    }

    #[test]
    fn truncated_frame_is_an_error() {
        let mut r: &[u8] = &[1, 0, 0, 0, 9, b'x'];
        assert!(read_frame(&mut r).is_err());
        let mut r: &[u8] = &[1, 0];
        assert!(read_frame(&mut r).is_err());
    }

    #[test]
    fn oversized_frames_are_rejected_and_chunked() {
        let mut r: &[u8] = &[1, 0xff, 0xff, 0xff, 0xff];
        assert!(read_frame(&mut r).is_err());
        let mut wire = Vec::new();
        write_chunks(&mut wire, 1, &vec![7; MAX_PAYLOAD + 1]).unwrap();
        let mut r = wire.as_slice();
        assert_eq!(
            read_frame(&mut r).unwrap().unwrap().payload.len(),
            MAX_PAYLOAD
        );
        assert_eq!(read_frame(&mut r).unwrap().unwrap().payload.len(), 1);
    }
}
