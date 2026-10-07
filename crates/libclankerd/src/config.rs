//! Machine configuration, split like Docker's `Config` / `HostConfig`.

use std::net::IpAddr;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::mount::Mount;

/// What runs: the image and how its main process is invoked.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MachineConfig {
    pub image: String,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    /// `KEY=value` entries.
    pub env: Vec<String>,
    pub user: String,
    pub working_dir: String,
    pub tty: bool,
    pub open_stdin: bool,
    /// Signal `stop` sends the workload (`SIGTERM`, `15`, ...); empty means
    /// the image's `StopSignal`, else SIGTERM.
    pub stop_signal: String,
}

impl MachineConfig {
    /// Names of the fields that differ from `other`.
    pub(crate) fn differing_fields(&self, other: &MachineConfig) -> Vec<&'static str> {
        [
            ("image", self.image != other.image),
            ("entrypoint", self.entrypoint != other.entrypoint),
            ("cmd", self.cmd != other.cmd),
            ("env", self.env != other.env),
            ("user", self.user != other.user),
            ("working_dir", self.working_dir != other.working_dir),
            ("tty", self.tty != other.tty),
            ("open_stdin", self.open_stdin != other.open_stdin),
            ("stop_signal", self.stop_signal != other.stop_signal),
        ]
        .into_iter()
        .filter_map(|(name, differs)| differs.then_some(name))
        .collect()
    }
}

/// When a machine whose workload exited is started again (Docker's `--restart`).
/// A machine stopped through the API is never restarted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RestartPolicy {
    #[default]
    No,
    Always,
    /// Restart only after a non-zero exit, at most `max_retries` times.
    OnFailure {
        max_retries: Option<u32>,
    },
    /// Like `Always`.
    UnlessStopped,
}

impl RestartPolicy {
    /// Whether a run that ended with `exit_code` (`-1`: vanished) is followed
    /// by a restart, given how many restarts already happened.
    pub(crate) fn restarts(&self, exit_code: i32, restarts_so_far: u32) -> bool {
        match self {
            RestartPolicy::No => false,
            RestartPolicy::Always | RestartPolicy::UnlessStopped => true,
            RestartPolicy::OnFailure { max_retries } => {
                exit_code != 0 && max_retries.is_none_or(|max| restarts_so_far < max)
            }
        }
    }
}

impl std::str::FromStr for RestartPolicy {
    type Err = String;

    /// `no`, `always`, `unless-stopped`, `on-failure[:N]`.
    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "no" => Ok(RestartPolicy::No),
            "always" => Ok(RestartPolicy::Always),
            "unless-stopped" => Ok(RestartPolicy::UnlessStopped),
            "on-failure" => Ok(RestartPolicy::OnFailure { max_retries: None }),
            _ => match s.strip_prefix("on-failure:") {
                Some(n) => n
                    .parse()
                    .map(|n| RestartPolicy::OnFailure {
                        max_retries: Some(n),
                    })
                    .map_err(|_| format!("invalid retry count \"{n}\" in restart policy")),
                None => Err(format!(
                    "invalid restart policy \"{s}\": use no, always, on-failure[:N] or unless-stopped"
                )),
            },
        }
    }
}

impl std::fmt::Display for RestartPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RestartPolicy::No => f.write_str("no"),
            RestartPolicy::Always => f.write_str("always"),
            RestartPolicy::UnlessStopped => f.write_str("unless-stopped"),
            RestartPolicy::OnFailure { max_retries: None } => f.write_str("on-failure"),
            RestartPolicy::OnFailure {
                max_retries: Some(n),
            } => write!(f, "on-failure:{n}"),
        }
    }
}

/// When create contacts the registry for an image (Docker's `--pull`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullPolicy {
    /// Pull only when the image is not cached.
    #[default]
    Missing,
    /// Pull every time.
    Always,
    /// Use the cache only; a missing image is `NotFound`.
    Never,
}

impl std::str::FromStr for PullPolicy {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        match s {
            "missing" => Ok(PullPolicy::Missing),
            "always" => Ok(PullPolicy::Always),
            "never" => Ok(PullPolicy::Never),
            _ => Err(format!(
                "invalid pull policy \"{s}\": use missing, always or never"
            )),
        }
    }
}

impl std::fmt::Display for PullPolicy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            PullPolicy::Missing => "missing",
            PullPolicy::Always => "always",
            PullPolicy::Never => "never",
        })
    }
}

/// How the machine is hosted: resources, mounts, ports, policies.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    pub cpus: Option<u32>,
    /// Bytes.
    pub memory: Option<u64>,
    /// Bind mounts and the machine's (at most one) named volume.
    pub mounts: Vec<Mount>,
    /// Guest ports published on host loopback, fixed for the machine's life.
    pub port_bindings: Vec<PortBinding>,
    /// Host loopback ports reachable at the same port on the guest's loopback.
    pub host_gateway_ports: Vec<u16>,
    /// Host unix sockets exposed at paths inside the guest.
    pub socket_bindings: Vec<SocketBinding>,
    pub restart_policy: RestartPolicy,
    /// Applies whenever the machine's image is resolved (create, image change).
    pub pull_policy: PullPolicy,
}

/// A guest TCP port published on the host. The host side is loopback only:
/// `host_ip` may name a loopback address but never anything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortBinding {
    /// Loopback address to listen on; `None` means 127.0.0.1.
    #[serde(default)]
    pub host_ip: Option<IpAddr>,
    /// `0` picks a free port (see `PublishedPort::host_port`).
    pub host_port: u16,
    pub guest_port: u16,
}

impl PortBinding {
    /// `127.0.0.1:host_port` -> guest `guest_port`.
    pub fn loopback(host_port: u16, guest_port: u16) -> Self {
        PortBinding {
            host_ip: None,
            host_port,
            guest_port,
        }
    }
}

/// A host unix socket exposed inside the guest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocketBinding {
    /// Where the guest sees the socket, e.g. `/var/run/clankerd.sock`.
    pub guest_path: PathBuf,
    /// The host socket it leads to.
    pub host_path: PathBuf,
}
