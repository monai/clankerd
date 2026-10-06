//! Host-side client for the guest's `io.clankerd.Guest` varlink interface.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Instant;

use clankerd_proto::guest::{
    ERROR_CONFLICT, ERROR_INVALID_PARAMETER, ERROR_NO_SUCH_EXEC, Event, METHOD_EVENTS,
};
use clankerd_proto::varlink::{self, Call, Reply};
use serde::Serialize;
use serde_json::Value;

use crate::error::{Error, Result};

/// A live `Events` stream.
pub(crate) struct EventStream {
    reader: BufReader<UnixStream>,
}

pub(crate) enum Next {
    Event(Event),
    /// The guest closed the stream (or sent garbage).
    Closed,
    TimedOut,
}

impl EventStream {
    /// Connects to the guest socket and subscribes to events.
    pub fn connect(socket: &Path) -> std::io::Result<Self> {
        let mut conn = UnixStream::connect(socket)?;
        varlink::write(
            &mut conn,
            &Call {
                method: METHOD_EVENTS.into(),
                parameters: serde_json::Value::Null,
                more: true,
                upgrade: false,
            },
        )?;
        Ok(EventStream {
            reader: BufReader::new(conn),
        })
    }

    /// Next event, waiting until `deadline` (forever when `None`).
    pub fn next(&mut self, deadline: Option<Instant>) -> Next {
        let timeout = match deadline {
            None => None,
            Some(d) => match d.checked_duration_since(Instant::now()) {
                Some(left) if !left.is_zero() => Some(left),
                _ => return Next::TimedOut,
            },
        };
        if self.reader.get_ref().set_read_timeout(timeout).is_err() {
            return Next::Closed;
        }
        match varlink::read::<Reply, _>(&mut self.reader) {
            Ok(Some(reply)) if reply.error.is_none() => {
                match serde_json::from_value(reply.parameters) {
                    Ok(event) => Next::Event(event),
                    Err(_) => Next::Closed,
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                Next::TimedOut
            }
            _ => Next::Closed,
        }
    }
}

/// Sends one call and returns the reply's parameters; guest errors map to [`Error`].
pub(crate) fn call(socket: &Path, method: &str, params: &impl Serialize) -> Result<Value> {
    Ok(send(socket, method, params, false)?.0)
}

/// Sends a call with `upgrade`. On success the returned reader carries the raw
/// framed stream (it may already hold buffered frames); the write half is
/// reachable through `get_ref`.
pub(crate) fn upgrade(
    socket: &Path,
    method: &str,
    params: &impl Serialize,
) -> Result<BufReader<UnixStream>> {
    Ok(send(socket, method, params, true)?.1)
}

fn send(
    socket: &Path,
    method: &str,
    params: &impl Serialize,
    upgrade: bool,
) -> Result<(Value, BufReader<UnixStream>)> {
    let mut conn = UnixStream::connect(socket)
        .map_err(|e| Error::unavailable(format!("cannot reach the guest: {e}")))?;
    varlink::write(
        &mut conn,
        &Call {
            method: method.into(),
            parameters: serde_json::to_value(params)?,
            more: false,
            upgrade,
        },
    )?;
    let mut reader = BufReader::new(conn);
    let reply: Reply = varlink::read(&mut reader)?
        .ok_or_else(|| Error::unavailable("the guest closed the connection"))?;
    match reply.error.as_deref() {
        None => Ok((reply.parameters, reader)),
        Some(name) => {
            let message = reply.parameters["message"]
                .as_str()
                .unwrap_or(name)
                .to_owned();
            Err(match name {
                ERROR_NO_SUCH_EXEC => Error::not_found(message),
                ERROR_INVALID_PARAMETER => Error::invalid_parameter(message),
                ERROR_CONFLICT => Error::conflict(message),
                _ => Error::system(format!("{name}: {message}")),
            })
        }
    }
}
