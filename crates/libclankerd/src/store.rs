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
use crate::image_config::ImageConfig;
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
    /// The image's runtime defaults at create time (`None` without an image cache).
    #[serde(default)]
    pub image_config: Option<ImageConfig>,
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
    write_atomic(path, &serde_json::to_vec_pretty(value)?)
}

/// Replaces `path` with `bytes` atomically: readers see the old or the new
/// content, never a partial file. The temp file name is unique per writer, so
/// concurrent writers (other threads, or another process over the same
/// directory) never share one.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let tmp = path.with_extension(format!(
        "tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let write = || -> std::io::Result<()> {
        fs::write(&tmp, bytes)?;
        fs::rename(&tmp, path)
    };
    write().inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn state_reads_never_see_a_torn_write_from_concurrent_writers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let record = Record {
            id: "a".repeat(64),
            name: "n".into(),
            created: SystemTime::now(),
            config: MachineConfig::default(),
            host_config: HostConfig::default(),
            image_id: String::new(),
            image_config: None,
        };
        store.create(&record, &MachineState::created()).unwrap();

        let stop = Arc::new(AtomicBool::new(false));
        // Two writers over one directory stand in for two processes.
        let writers: Vec<_> = (0..2)
            .map(|_| {
                let (store, id, stop) = (store.clone(), record.id.clone(), stop.clone());
                std::thread::spawn(move || {
                    let mut state = MachineState::created();
                    state.error = "x".repeat(4096);
                    while !stop.load(Ordering::Relaxed) {
                        store.save_state(&id, &state).unwrap();
                    }
                })
            })
            .collect();
        for _ in 0..3000 {
            store.load_state(&record.id).expect("a whole state record");
        }
        stop.store(true, Ordering::Relaxed);
        for w in writers {
            w.join().unwrap();
        }
    }
}
