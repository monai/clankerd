//! Host-side client for the guest's `io.clankerd.Guest` varlink interface.

use std::io::BufReader;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Instant;

use clankerd_proto::guest::{Event, METHOD_EVENTS};
use clankerd_proto::varlink::{self, Call, Reply};

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
