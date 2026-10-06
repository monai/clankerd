//! On-disk persistence: `<state>/machines/<id>/{config.json,state.json}`.
//!
//! Writes are atomic (temp file + rename) so a crash never leaves a torn record.
//! Callers serialise read-modify-write cycles themselves.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::config::{HostConfig, MachineConfig};
use crate::error::{Error, Result};
use crate::state::MachineState;

/// The immutable part of a machine record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    pub id: String,
    pub name: String,
    pub created: SystemTime,
    pub config: MachineConfig,
    pub host_config: HostConfig,
    /// Digest the image reference was pinned to at create time (`sha256:...`);
    /// empty when the engine has no image cache.
    #[serde(default)]
    pub image_id: String,
}

#[derive(Debug, Clone)]
pub struct Store {
    machines: PathBuf,
}

impl Store {
    pub fn open(state_dir: &Path) -> Result<Self> {
        let machines = state_dir.join("machines");
        fs::create_dir_all(&machines)?;
        Ok(Store { machines })
    }

    pub fn machine_dir(&self, id: &str) -> PathBuf {
        self.machines.join(id)
    }

    pub fn ids(&self) -> Result<Vec<String>> {
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.machines)? {
            let entry = entry?;
            if entry.path().join("config.json").is_file() {
                ids.push(entry.file_name().to_string_lossy().into_owned());
            }
        }
        ids.sort();
        Ok(ids)
    }

    pub fn create(&self, record: &Record, state: &MachineState) -> Result<()> {
        let dir = self.machine_dir(&record.id);
        fs::create_dir_all(&dir)?;
        write_json(&dir.join("config.json"), record)?;
        self.save_state(&record.id, state)
    }

    pub fn load(&self, id: &str) -> Result<(Record, MachineState)> {
        let dir = self.machine_dir(id);
        Ok((
            read_json(&dir.join("config.json"), id)?,
            read_json(&dir.join("state.json"), id)?,
        ))
    }

    pub fn load_state(&self, id: &str) -> Result<MachineState> {
        read_json(&self.machine_dir(id).join("state.json"), id)
    }

    pub fn save_state(&self, id: &str, state: &MachineState) -> Result<()> {
        write_json(&self.machine_dir(id).join("state.json"), state)
    }

    pub fn remove(&self, id: &str) -> Result<()> {
        fs::remove_dir_all(self.machine_dir(id))?;
        Ok(())
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path, id: &str) -> Result<T> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Err(Error::not_found(format!("no such machine: {id}")))
        }
        Err(e) => Err(e.into()),
    }
}
