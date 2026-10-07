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
    /// Docker `user[:group]` spec resolved against the image's passwd and
    /// group files; empty runs as root.
    #[serde(default)]
    pub user: String,
    /// The host's wall clock when the machine was started; guestd sets the
    /// guest clock from it when it boots a root disk.
    #[serde(default)]
    pub clock: Option<Clock>,
    /// Present when the machine has a virtio NIC: what guestd configures on it.
    #[serde(default)]
    pub network: Option<NetworkConfig>,
}

/// Guest network setup done before the workload starts (DHCP, resolv.conf,
/// hostname, forwarding sysctls and the firewall rules below).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkConfig {
    /// The interface to run DHCP on (`eth0`).
    pub interface: String,
    pub hostname: String,
    /// Network-backend addresses the guest, and containers behind it, must not
    /// reach (the backend's control API, host loopback aliases). guestd drops
    /// them in nftables before configuring the interface.
    #[serde(default)]
    pub blocked: Vec<BlockedEndpoint>,
}

/// An IPv4 destination to drop: one TCP port, or every protocol when `tcp_port` is `None`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedEndpoint {
    pub addr: std::net::Ipv4Addr,
    #[serde(default)]
    pub tcp_port: Option<u16>,
}

/// A point in time as seconds and nanoseconds since the Unix epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clock {
    pub secs: i64,
    pub nanos: u32,
}

impl Clock {
    pub fn now() -> Self {
        let d = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Clock {
            secs: d.as_secs() as i64,
            nanos: d.subsec_nanos(),
        }
    }
}

/// Graceful stop: signals the workload with SIGTERM and returns at once. The
/// machine then follows its normal end (the workload's exit is reported through
/// `Events`, then processes are stopped, disks synced and unmounted and the
/// machine powered off). The caller escalates to killing the VMM on a timeout.
pub const METHOD_SHUTDOWN: &str = "io.clankerd.Guest.Shutdown";
/// Signals the workload (parameters: [`SignalParams`], `id` unused).
pub const METHOD_KILL: &str = "io.clankerd.Guest.Kill";

/// Parameters of each reply of [`METHOD_EVENTS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "lowercase")]
pub enum Event {
    Ready,
    Exited { exit_code: i32 },
}

/// Exec lifecycle (Docker naming). `ExecStart` is called with `upgrade`: after
/// its reply the connection carries frames (see [`crate::frame`]).
pub const METHOD_EXEC_CREATE: &str = "io.clankerd.Guest.ExecCreate";
pub const METHOD_EXEC_START: &str = "io.clankerd.Guest.ExecStart";
pub const METHOD_EXEC_RESIZE: &str = "io.clankerd.Guest.ExecResize";
pub const METHOD_EXEC_KILL: &str = "io.clankerd.Guest.ExecKill";
pub const METHOD_EXEC_INSPECT: &str = "io.clankerd.Guest.ExecInspect";

pub const ERROR_NO_SUCH_EXEC: &str = "io.clankerd.Guest.NoSuchExec";
pub const ERROR_INVALID_PARAMETER: &str = "io.clankerd.Guest.InvalidParameter";
pub const ERROR_CONFLICT: &str = "io.clankerd.Guest.Conflict";

/// Parameters of `ExecCreate`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExecSpec {
    pub argv: Vec<String>,
    /// `KEY=value` entries, layered over the machine's environment.
    #[serde(default)]
    pub env: Vec<String>,
    #[serde(default)]
    pub working_dir: String,
    /// Docker `user[:group]` spec; empty means the machine's user (the workload's).
    #[serde(default)]
    pub user: String,
    #[serde(default)]
    pub tty: bool,
    #[serde(default)]
    pub attach_stdin: bool,
    /// Initial terminal size (tty only).
    #[serde(default)]
    pub size: Option<ResizeParams>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecCreated {
    pub id: String,
}

/// Parameters of `ExecStart`, `ExecInspect`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecRef {
    pub id: String,
}

/// Parameters of `ExecResize` and payload of the resize channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResizeParams {
    #[serde(default)]
    pub id: String,
    pub rows: u16,
    pub cols: u16,
}

/// Parameters of `ExecKill` and payload of the signal channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalParams {
    #[serde(default)]
    pub id: String,
    pub signal: i32,
}

/// Reply of `ExecInspect`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecState {
    pub running: bool,
    /// `None` until the process exited.
    pub exit_code: Option<i32>,
    /// Set when the process died from a signal (exit code is then 128+signal).
    pub signal: Option<i32>,
    pub pid: Option<i32>,
}

/// Payload of the status channel, the last frame before the stream closes.
/// Always sent after all output frames.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecStatus {
    pub exit_code: i32,
    #[serde(default)]
    pub signal: Option<i32>,
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
