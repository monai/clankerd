//! Machine state, following Docker's `State`.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Created,
    Running,
    Restarting,
    Removing,
    Exited,
    /// The machine vanished without reporting an exit code.
    Dead,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Status::Created => "created",
            Status::Running => "running",
            Status::Restarting => "restarting",
            Status::Removing => "removing",
            Status::Exited => "exited",
            Status::Dead => "dead",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineState {
    pub status: Status,
    /// Of the last run; `-1` when unknown (status `dead`).
    pub exit_code: i32,
    pub oom_killed: bool,
    /// Why the last start failed, if it did.
    pub error: String,
    /// Process id of the VMM helper while running.
    pub pid: Option<u32>,
    pub started_at: Option<SystemTime>,
    pub finished_at: Option<SystemTime>,
}

impl MachineState {
    pub fn created() -> Self {
        MachineState {
            status: Status::Created,
            exit_code: 0,
            oom_killed: false,
            error: String::new(),
            pid: None,
            started_at: None,
            finished_at: None,
        }
    }
}

/// Result of [`crate::Machine::wait`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitResult {
    pub exit_code: i32,
}
