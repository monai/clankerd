//! Machine configuration, split like Docker's `Config` / `HostConfig`.

use serde::{Deserialize, Serialize};

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
}

/// How the machine is hosted: resources, mounts, ports, policies.
/// Mounts, port bindings, restart and pull policy arrive with later tickets.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostConfig {
    pub cpus: Option<u32>,
    /// Bytes.
    pub memory: Option<u64>,
}
