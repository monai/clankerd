//! The `io.clankerd.Guest` varlink interface served by clankerd-guestd.

use serde::{Deserialize, Serialize};

pub const INTERFACE: &str = "io.clankerd.Guest";

/// Streaming method: replies `ready` first, then `exited` when the workload ends.
pub const METHOD_EVENTS: &str = "io.clankerd.Guest.Events";

pub const ERROR_METHOD_NOT_FOUND: &str = "org.varlink.service.MethodNotFound";

/// What guestd runs as the machine's main process.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Workload {
    /// Entrypoint followed by cmd, already combined.
    pub argv: Vec<String>,
    /// `KEY=value` entries.
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub working_dir: String,
}

/// Parameters of each reply of [`METHOD_EVENTS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Event {
    Ready,
    Exited { exit_code: i32 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_use_the_documented_shape() {
        assert_eq!(
            serde_json::to_string(&Event::Exited { exit_code: 3 }).unwrap(),
            r#"{"event":"exited","exit_code":3}"#
        );
        assert_eq!(
            serde_json::from_str::<Event>(r#"{"event":"ready"}"#).unwrap(),
            Event::Ready
        );
    }
}
