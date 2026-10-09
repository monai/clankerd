//! Named volumes: `<state>/volumes/<name>/{volume.json,data.ext4}`.
//!
//! `data.ext4` is a sparse raw file. The host never creates a filesystem in it
//! (no ext4 tools on the Mac): clankerd-guestd formats it when blank, grows
//! the filesystem when the file grew, and mounts it. A volume lives outside
//! any machine directory, so removing a machine keeps it.

use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Size of a new volume that does not name one (sparse, so it costs nothing).
pub const DEFAULT_SIZE: u64 = 16 << 30;
/// Smallest volume mke2fs can sensibly format.
pub const MIN_SIZE: u64 = 1 << 20;

const DATA_FILE: &str = "data.ext4";
const META_FILE: &str = "volume.json";

/// What `inspect` and `list` report about a volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VolumeInfo {
    pub name: String,
    /// Host path of the sparse raw image.
    pub path: PathBuf,
    /// Bytes: the size of the image (and of the filesystem once grown).
    pub size: u64,
    pub created: SystemTime,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    created: SystemTime,
    #[serde(default)]
    size: Option<u64>,
}

pub(crate) fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

#[derive(Debug, Clone)]
pub(crate) struct VolumeStore {
    root: PathBuf,
}

impl VolumeStore {
    pub fn new(root: &Path) -> Self {
        VolumeStore {
            root: root.to_path_buf(),
        }
    }

    fn dir(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }

    pub fn data_path(&self, name: &str) -> PathBuf {
        self.dir(name).join(DATA_FILE)
    }

    pub fn exists(&self, name: &str) -> bool {
        self.dir(name).join(META_FILE).is_file()
    }

    /// Creates a sparse volume of `size` bytes (default [`DEFAULT_SIZE`]).
    pub fn create(&self, name: &str, size: Option<u64>) -> Result<VolumeInfo> {
        if !valid_name(name) {
            return Err(Error::invalid_parameter(format!(
                "invalid volume name \"{name}\": must match [a-zA-Z0-9][a-zA-Z0-9_.-]*"
            )));
        }
        let size = size.unwrap_or(DEFAULT_SIZE);
        if size < MIN_SIZE {
            return Err(Error::invalid_parameter(format!(
                "volume size {size} is below the minimum of 1M"
            )));
        }
        fs::create_dir_all(&self.root)?;
        let dir = self.dir(name);
        match fs::create_dir(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(Error::conflict(format!("volume \"{name}\" already exists")));
            }
            Err(e) => return Err(e.into()),
        }
        let made = (|| -> Result<()> {
            File::create(self.data_path(name))?.set_len(size)?;
            let meta = Meta {
                created: SystemTime::now(),
                size: Some(size),
            };
            // The metadata file marks the volume complete, so it goes last.
            crate::store::write_atomic(&dir.join(META_FILE), &serde_json::to_vec(&meta)?)
        })();
        if let Err(e) = made {
            let _ = fs::remove_dir_all(&dir);
            return Err(e);
        }
        self.get(name)
    }

    pub fn get(&self, name: &str) -> Result<VolumeInfo> {
        let not_found = || Error::not_found(format!("no such volume: {name}"));
        if !valid_name(name) {
            return Err(not_found());
        }
        let meta = match fs::read(self.dir(name).join(META_FILE)) {
            Ok(b) => serde_json::from_slice::<Meta>(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(not_found()),
            Err(e) => return Err(e.into()),
        };
        let path = self.data_path(name);
        let actual = fs::metadata(&path)?.len();
        let capacity = meta.size.unwrap_or_else(|| {
            crate::rootdisk::ext4_capacity(&path)
                .unwrap_or(actual)
                .max(actual)
        });
        Ok(VolumeInfo {
            name: name.to_owned(),
            size: capacity.max(actual),
            path,
            created: meta.created,
        })
    }

    /// Volumes by name.
    pub fn list(&self) -> Result<Vec<VolumeInfo>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
        };
        let mut out = Vec::new();
        for entry in entries {
            if let Ok(info) = self.get(&entry?.file_name().to_string_lossy()) {
                out.push(info);
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    pub fn remove(&self, name: &str) -> Result<()> {
        self.get(name)?;
        fs::remove_dir_all(self.dir(name))?;
        Ok(())
    }

    /// Makes the image at least `want` bytes (never shrinks) and returns its
    /// size. The guest grows the filesystem into the new space at boot.
    pub fn grow_to(&self, name: &str, want: Option<u64>) -> Result<u64> {
        let info = self.get(name)?;
        let capacity = info.size.max(want.unwrap_or(info.size));
        let mut meta: Meta = serde_json::from_slice(&fs::read(self.dir(name).join(META_FILE))?)?;
        meta.size = Some(capacity);
        crate::store::write_atomic(&self.dir(name).join(META_FILE), &serde_json::to_vec(&meta)?)?;
        if fs::metadata(&info.path)?.len() < capacity {
            OpenOptions::new()
                .write(true)
                .open(&info.path)?
                .set_len(capacity)?;
        }
        Ok(capacity)
    }
}

/// Names of the machines (other than `except`) whose configuration mounts `volume`.
pub(crate) fn users_of(
    inner: &crate::engine::Inner,
    volume: &str,
    except: Option<&str>,
) -> Result<Vec<String>> {
    let mut names = Vec::new();
    for id in inner.store.ids()? {
        if Some(id.as_str()) == except {
            continue;
        }
        if let Ok((record, _)) = inner.store.load(&id)
            && crate::mount::volume_name(&record.host_config.mounts) == Some(volume)
        {
            names.push(record.name);
        }
    }
    Ok(names)
}
