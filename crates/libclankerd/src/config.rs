//! Machine configuration, split like Docker's `Config` / `HostConfig`.

pub use clankerd_proto::tunnel::SocketBinding;

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

/// When an ended machine is started again (Docker-style `--restart` syntax).
/// An OCI command exiting does not end or restart the machine.
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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetworkMode {
    #[default]
    Shared,
    HostOnly,
    Bridged,
}

impl std::str::FromStr for NetworkMode {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "shared" => Ok(Self::Shared),
            "bridged" => Ok(Self::Bridged),
            "host-only" => Ok(Self::HostOnly),
            _ => Err("network mode must be shared, host-only or bridged".into()),
        }
    }
}

/// macOS 15 selects a common vmnet network by its private IPv4 subnet.
/// IPv6 configuration remains automatic and has no global family preference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SharedNetwork {
    pub address: std::net::Ipv4Addr,
    pub prefix_len: u8,
}
impl Default for SharedNetwork {
    fn default() -> Self {
        Self {
            address: std::net::Ipv4Addr::new(192, 168, 105, 0),
            prefix_len: 24,
        }
    }
}
impl SharedNetwork {
    pub(crate) fn addresses(
        &self,
    ) -> crate::Result<(std::net::Ipv4Addr, std::net::Ipv4Addr, std::net::Ipv4Addr)> {
        use std::net::Ipv4Addr;
        if !(8..=30).contains(&self.prefix_len) {
            return Err(crate::Error::invalid_parameter(
                "shared network requires an RFC1918 IPv4 subnet with prefix /8 through /30",
            ));
        }
        let mask = u32::MAX << (32 - self.prefix_len);
        let base = u32::from(self.address);
        let end = Ipv4Addr::from(base | !mask);
        if base & !mask != 0 || !self.address.is_private() || !end.is_private() {
            return Err(crate::Error::invalid_parameter(
                "shared network must be an aligned RFC1918 IPv4 subnet",
            ));
        }
        Ok((
            Ipv4Addr::from(base + 1),
            Ipv4Addr::from(u32::from(end) - 1),
            Ipv4Addr::from(mask),
        ))
    }
}
impl std::str::FromStr for SharedNetwork {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        let (address, prefix) = text
            .split_once('/')
            .ok_or_else(|| "shared network needs IPv4 CIDR, e.g. 192.168.109.0/24".to_owned())?;
        let network = Self {
            address: address
                .parse()
                .map_err(|_| "shared network needs an IPv4 address")?,
            prefix_len: prefix
                .parse()
                .map_err(|_| "shared network needs a numeric prefix")?,
        };
        network
            .addresses()
            .map_err(|error| error.message().to_owned())?;
        Ok(network)
    }
}

/// How the machine is hosted: resources, mounts, ports, policies.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct HostConfig {
    /// Canonical virtual network membership; bridged attachments have none.
    pub virtual_network: Option<String>,
    pub network_mode: NetworkMode,
    /// Explicit physical interface for bridged networking; never follows the host default route.
    pub bridge_interface: Option<String>,
    pub shared_network: SharedNetwork,
    /// Authoritative native shared ULA /64, copied from virtual-network membership.
    pub shared_ipv6_prefix: Option<std::net::Ipv6Addr>,
    pub cpus: Option<u32>,
    /// Bytes.
    pub memory: Option<u64>,
    /// Bind mounts and the machine's (at most one) named volume.
    pub mounts: Vec<Mount>,
    /// Host unix sockets exposed at paths inside the guest.
    pub socket_bindings: Vec<SocketBinding>,
    pub restart_policy: RestartPolicy,
    /// Applies whenever the machine's image is resolved (create, image change).
    pub pull_policy: PullPolicy,
}

pub(crate) fn validate_bridge_config(host: &HostConfig) -> crate::error::Result<()> {
    if host.network_mode == NetworkMode::Bridged && host.shared_network != SharedNetwork::default()
    {
        return Err(crate::error::Error::invalid_parameter(
            "shared network selection does not apply to a bridged physical interface",
        ));
    }
    match (&host.network_mode, &host.bridge_interface) {
        (NetworkMode::Bridged, Some(interface))
            if !interface.is_empty()
                && interface.len() < libc::IFNAMSIZ
                && interface
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')) =>
        {
            Ok(())
        }
        (NetworkMode::Bridged, _) => Err(crate::error::Error::invalid_parameter(
            "bridged networking requires an explicit physical bridge interface name",
        )),
        (_, Some(_)) => Err(crate::error::Error::invalid_parameter(
            "bridge interface is only valid for bridged networking",
        )),
        (_, None) => Ok(()),
    }
}
