//! Minimal varlink wire format: each message is one JSON object followed by a NUL byte.

use std::io::{self, BufRead, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A method call.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Call {
    pub method: String,
    #[serde(default)]
    pub parameters: Value,
    /// Caller accepts several replies (a stream).
    #[serde(default, skip_serializing_if = "is_false")]
    pub more: bool,
}

/// A method reply. `continues` is set on every reply of a stream but the last.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub parameters: Value,
    #[serde(default, skip_serializing_if = "is_false")]
    pub continues: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// Writes one message and flushes.
pub fn write<T: Serialize, W: Write>(w: &mut W, msg: &T) -> io::Result<()> {
    let mut buf = serde_json::to_vec(msg).map_err(io::Error::other)?;
    buf.push(0);
    w.write_all(&buf)?;
    w.flush()
}

/// Reads one message; `Ok(None)` on a clean end of stream.
pub fn read<T: DeserializeOwned, R: BufRead>(r: &mut R) -> io::Result<Option<T>> {
    let mut buf = Vec::new();
    if r.read_until(0, &mut buf)? == 0 {
        return Ok(None);
    }
    if buf.last() != Some(&0) {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    buf.pop();
    serde_json::from_slice(&buf)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip_nul_terminated() {
        let mut wire = Vec::new();
        write(
            &mut wire,
            &Call {
                method: "a.b.C".into(),
                parameters: Value::Null,
                more: true,
            },
        )
        .unwrap();
        assert_eq!(wire.last(), Some(&0));
        let call: Call = read(&mut wire.as_slice()).unwrap().unwrap();
        assert_eq!(call.method, "a.b.C");
        assert!(call.more);
    }

    #[test]
    fn truncated_message_is_an_error() {
        let mut r: &[u8] = br#"{"method":"x""#;
        assert!(read::<Call, _>(&mut r).is_err());
    }
}
