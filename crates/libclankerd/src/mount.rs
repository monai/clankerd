//! Mounts: the [`Mount`] configuration type, its validation, and the plan that
//! turns a machine's mounts into what the VMM attaches (a volume disk, virtio-fs
//! shares) and what guestd mounts (`GuestMount`).

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use clankerd_proto::guest::GuestMount;
use clankerd_proto::spawn::{Share, VOLUME_DEVICE};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::volumes::{DEFAULT_SIZE, MIN_SIZE, VolumeStore, valid_name};

/// A mount inside the guest: a host directory or a named volume.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Mount {
    /// A host directory shared over virtio-fs.
    Bind {
        /// Absolute host path of an existing directory.
        source: PathBuf,
        /// Absolute guest path.
        target: PathBuf,
        #[serde(default)]
        read_only: bool,
    },
    /// A named volume: a sparse raw ext4 file in the engine's state directory,
    /// formatted by the guest when blank. Created with the machine if missing.
    Volume {
        name: String,
        /// Absolute guest path.
        target: PathBuf,
        /// Docker's `driver_opts`; `size` is the volume's size in bytes, with
        /// an optional K/M/G/T suffix (a new volume defaults to 16G). A larger
        /// size grows an existing volume at the next start; a smaller one is
        /// ignored (volumes never shrink).
        #[serde(default)]
        driver_opts: BTreeMap<String, String>,
    },
}

impl Mount {
    pub fn bind(source: impl Into<PathBuf>, target: impl Into<PathBuf>) -> Self {
        Mount::Bind {
            source: source.into(),
            target: target.into(),
            read_only: false,
        }
    }

    pub fn volume(name: impl Into<String>, target: impl Into<PathBuf>) -> Self {
        Mount::Volume {
            name: name.into(),
            target: target.into(),
            driver_opts: BTreeMap::new(),
        }
    }

    /// Sets a volume's `size` option in bytes; no effect on a bind mount.
    pub fn with_size(mut self, bytes: u64) -> Self {
        if let Mount::Volume { driver_opts, .. } = &mut self {
            driver_opts.insert("size".into(), bytes.to_string());
        }
        self
    }

    pub fn target(&self) -> &Path {
        match self {
            Mount::Bind { target, .. } | Mount::Volume { target, .. } => target,
        }
    }

    /// The size a volume mount asks for, if it names one.
    pub(crate) fn requested_size(&self) -> Result<Option<u64>> {
        match self {
            Mount::Volume { driver_opts, .. } => {
                driver_opts.get("size").map(|s| parse_size(s)).transpose()
            }
            Mount::Bind { .. } => Ok(None),
        }
    }
}

/// Parses a size: an integer with an optional K/M/G/T suffix (powers of 1024,
/// optionally followed by `B` or `iB`), e.g. `512M`, `20G`.
pub fn parse_size(text: &str) -> Result<u64> {
    let bad = || Error::invalid_parameter(format!("invalid size \"{text}\": expected e.g. 20G"));
    let t = text.trim();
    let t = t
        .strip_suffix("iB")
        .or_else(|| t.strip_suffix('B'))
        .or_else(|| t.strip_suffix("ib"))
        .or_else(|| t.strip_suffix('b'))
        .unwrap_or(t);
    let (digits, shift) = match t.chars().last() {
        Some('k' | 'K') => (&t[..t.len() - 1], 10),
        Some('m' | 'M') => (&t[..t.len() - 1], 20),
        Some('g' | 'G') => (&t[..t.len() - 1], 30),
        Some('t' | 'T') => (&t[..t.len() - 1], 40),
        _ => (t, 0),
    };
    let n: u64 = digits.parse().map_err(|_| bad())?;
    n.checked_mul(1 << shift).ok_or_else(bad)
}

fn valid_guest_path(p: &Path) -> bool {
    p.is_absolute()
        && p != Path::new("/")
        && p.components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
}

/// Checks mounts without touching the disk, except that bind sources must be
/// existing directories.
pub(crate) fn validate(mounts: &[Mount]) -> Result<()> {
    let mut volumes = 0;
    let mut targets: Vec<&Path> = Vec::new();
    for m in mounts {
        let target = m.target();
        if !valid_guest_path(target) {
            return Err(Error::invalid_parameter(format!(
                "invalid mount target \"{}\": must be an absolute path without . or .. parts",
                target.display()
            )));
        }
        if targets.contains(&target) {
            return Err(Error::invalid_parameter(format!(
                "duplicate mount target {}",
                target.display()
            )));
        }
        targets.push(target);
        match m {
            Mount::Bind { source, .. } => check_bind_source(source)?,
            Mount::Volume { name, .. } => {
                volumes += 1;
                if volumes > 1 {
                    return Err(Error::invalid_parameter(
                        "a machine can have at most one named volume",
                    ));
                }
                if !valid_name(name) {
                    return Err(Error::invalid_parameter(format!(
                        "invalid volume name \"{name}\": must match [a-zA-Z0-9][a-zA-Z0-9_.-]*"
                    )));
                }
                if let Some(size) = m.requested_size()?
                    && size < MIN_SIZE
                {
                    return Err(Error::invalid_parameter(format!(
                        "volume size {size} is below the minimum of 1M"
                    )));
                }
            }
        }
    }
    Ok(())
}

fn check_bind_source(source: &Path) -> Result<()> {
    if !source.is_absolute() {
        return Err(Error::invalid_parameter(format!(
            "bind mount source {} must be an absolute path",
            source.display()
        )));
    }
    if !source.is_dir() {
        return Err(Error::invalid_parameter(format!(
            "bind mount source {} is not an existing directory",
            source.display()
        )));
    }
    Ok(())
}

/// Name of the machine's volume, if it mounts one.
pub(crate) fn volume_name(mounts: &[Mount]) -> Option<&str> {
    mounts.iter().find_map(|m| match m {
        Mount::Volume { name, .. } => Some(name.as_str()),
        Mount::Bind { .. } => None,
    })
}

/// What a boot needs for a machine's mounts.
#[derive(Debug, Default)]
pub(crate) struct MountPlan {
    pub volume_disk: Option<PathBuf>,
    pub shares: Vec<Share>,
    pub guest: Vec<GuestMount>,
}

/// Creates the machine's volume if it does not exist yet; returns whether it did.
pub(crate) fn ensure_volume(volumes: &VolumeStore, mounts: &[Mount]) -> Result<bool> {
    for m in mounts {
        if let Mount::Volume { name, .. } = m {
            if volumes.exists(name) {
                return Ok(false);
            }
            volumes.create(name, Some(m.requested_size()?.unwrap_or(DEFAULT_SIZE)))?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// Prepares the mounts for a boot: grows the volume file to the size asked
/// for (never shrinks), checks bind sources, assigns virtio-fs tags.
pub(crate) fn plan(
    volumes: &VolumeStore,
    mounts: &[Mount],
    has_root_disk: bool,
) -> Result<MountPlan> {
    let mut plan = MountPlan::default();
    for m in mounts {
        match m {
            Mount::Volume { name, target, .. } => {
                let size = volumes.grow_to(name, m.requested_size()?)?;
                plan.volume_disk = Some(volumes.data_path(name));
                plan.guest.push(GuestMount::Volume {
                    // Without a root disk the volume is the first block device.
                    device: if has_root_disk {
                        VOLUME_DEVICE.into()
                    } else {
                        clankerd_proto::spawn::ROOT_DEVICE.into()
                    },
                    target: target.to_string_lossy().into_owned(),
                    size,
                });
            }
            Mount::Bind {
                source,
                target,
                read_only,
            } => {
                check_bind_source(source)?;
                let tag = format!("bind{}", plan.shares.len());
                plan.shares.push(Share {
                    tag: tag.clone(),
                    path: source.clone(),
                });
                plan.guest.push(GuestMount::Bind {
                    tag,
                    target: target.to_string_lossy().into_owned(),
                    read_only: *read_only,
                });
            }
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_volume_follows_the_root_disk_as_the_second_block_device() {
        let dir = tempfile::tempdir().unwrap();
        let volumes = VolumeStore::new(dir.path());
        volumes.create("data", Some(MIN_SIZE)).unwrap();
        let mounts = [Mount::volume("data", "/storage")];
        let device = |root: bool| match &plan(&volumes, &mounts, root).unwrap().guest[0] {
            GuestMount::Volume { device, .. } => device.clone(),
            other => panic!("{other:?}"),
        };
        assert_eq!(device(true), "/dev/vdb");
        assert_eq!(device(false), "/dev/vda");
    }

    #[test]
    fn sizes_parse_with_binary_suffixes() {
        for (text, want) in [
            ("1048576", 1 << 20),
            ("512M", 512 << 20),
            ("20G", 20 << 30),
            ("20g", 20 << 30),
            ("1T", 1 << 40),
            ("64KiB", 64 << 10),
            ("2MB", 2 << 20),
        ] {
            assert_eq!(parse_size(text).unwrap(), want, "{text}");
        }
        for text in ["", "G", "-1G", "1.5G", "lots", "99999999999999T"] {
            assert!(parse_size(text).is_err(), "{text}");
        }
    }
}
