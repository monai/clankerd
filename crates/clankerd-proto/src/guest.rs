//! The `io.clankerd.Guest` varlink interface served by clankerd-guestd.

use serde::{Deserialize, Serialize};

pub const INTERFACE: &str = "io.clankerd.Guest";

/// Streaming method: replies `ready` first, then `exited` when the workload ends.
pub const METHOD_EVENTS: &str = "io.clankerd.Guest.Events";
/// The library has completed startup and can recover the workload's exit result.
pub const METHOD_STARTUP_COMPLETE: &str = "io.clankerd.Guest.StartupComplete";
/// Confirms the helper recorded an exit code, or an attached client received it.
pub const METHOD_ACKNOWLEDGE_EXIT: &str = "io.clankerd.Guest.AcknowledgeExit";

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExitObserver {
    Recorded,
    Attached,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct ExitAcknowledgment {
    pub exit_code: i32,
    pub observer: ExitObserver,
}

/// Sends an exit acknowledgment while keeping the connection open for delivery.
/// Power-off may close the reply transport after the guest consumes the acknowledgment.
pub fn acknowledge_exit(socket: &std::path::Path, ack: ExitAcknowledgment) -> std::io::Result<()> {
    use crate::varlink::{self, Call, Reply};
    use std::io::{self, BufReader};
    use std::os::unix::net::UnixStream;
    let mut connection = UnixStream::connect(socket)?;
    connection.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    varlink::write(
        &mut connection,
        &Call {
            method: METHOD_ACKNOWLEDGE_EXIT.into(),
            parameters: serde_json::to_value(ack)?,
            more: false,
            upgrade: false,
        },
    )?;
    let reply = match varlink::read::<Reply, _>(&mut BufReader::new(connection)) {
        Ok(reply) => reply,
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::ConnectionReset | io::ErrorKind::UnexpectedEof
            ) =>
        {
            None
        }
        Err(e) => return Err(e),
    };
    match reply {
        Some(reply) if reply.error.is_some() => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "guest rejected exit acknowledgment",
        )),
        _ => Ok(()),
    }
}

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
    /// Volume and bind mounts guestd sets up after pivoting into the root disk.
    #[serde(default)]
    pub mounts: Vec<GuestMount>,
    /// Present when the machine has a virtio NIC: what guestd configures on it.
    #[serde(default)]
    pub network: Option<NetworkConfig>,
    /// Run the workload on a pseudo-terminal that `Attach` connects to.
    #[serde(default)]
    pub tty: bool,
    /// Give the workload a stdin that `Attach` feeds. With neither this nor
    /// `tty` the workload's output goes to the console only.
    #[serde(default)]
    pub open_stdin: bool,
    /// Reserve a foreground attachment until its client consumes terminal status.
    #[serde(default)]
    pub expect_attach: bool,
}

/// One mount guestd performs at boot (`--boot` only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum GuestMount {
    /// A sparse raw ext4 file attached as a block device: formatted when
    /// blank, grown to `size` bytes when smaller, mounted at `target`.
    Volume {
        device: String,
        target: String,
        size: u64,
    },
    /// A host directory shared over virtio-fs under `tag`.
    Bind {
        tag: String,
        target: String,
        #[serde(default)]
        read_only: bool,
    },
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

/// Graceful stop: signals the workload (parameters: [`ShutdownParams`]) and
/// returns at once. The
/// machine then follows its normal end (the workload's exit is reported through
/// `Events`, then processes are stopped, disks synced and unmounted and the
/// machine powered off). The caller escalates to killing the VMM on a timeout.
pub const METHOD_SHUTDOWN: &str = "io.clankerd.Guest.Shutdown";
/// Parameters of [`METHOD_SHUTDOWN`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownParams {
    /// The machine's stop signal.
    #[serde(default = "default_stop_signal")]
    pub signal: i32,
}

fn default_stop_signal() -> i32 {
    15
}

/// Sets the guest's wall clock (parameters: [`Clock`]); the host calls it after
/// it woke from sleep, when the guest clock stopped with the host.
pub const METHOD_SET_CLOCK: &str = "io.clankerd.Guest.SetClock";
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

/// Connects to the workload's terminal or stdio (Docker's attach). Called with
/// `upgrade` like `ExecStart`, so the connection then carries frames; parameters:
/// [`AttachParams`]. Output produced before the first attach is replayed. Hanging
/// up detaches without touching the workload; the status frame ends the stream
/// when the workload exits. Needs `Workload::tty` or `Workload::open_stdin`.
pub const METHOD_ATTACH: &str = "io.clankerd.Guest.Attach";

/// Parameters of [`METHOD_ATTACH`].
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AttachParams {
    /// Terminal size `(rows, cols)` to set when the workload has a terminal.
    #[serde(default)]
    pub size: Option<ResizeParams>,
}

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
