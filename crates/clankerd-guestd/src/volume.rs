//! The machine's volume and bind mounts, set up by guestd right after it
//! pivoted into the root disk (`--boot`).
//!
//! A volume is a sparse raw ext4 file the host attached as a block device. It
//! is formatted here when blank (static `mke2fs` from the boot directory, so no
//! ext4 tools are needed on the Mac), grown with `resize2fs` when the host's
//! desired size exceeds the filesystem, and mounted where the configuration
//! says. Bind mounts are virtio-fs shares mounted by tag.
//!
//! Format and grow work on regular files, so they are unit tested without
//! root; mounting needs root (and virtio-fs needs a VM), so the mount tests
//! skip themselves unprivileged and the virtio-fs mount is verified on the M2.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::process::Command;

use clankerd_proto::guest::GuestMount;

use crate::rootdisk::{Ctx, format_device};

const SUPERBLOCK: u64 = 1024;
const EXT4_MAGIC: u16 = 0xEF53;
const INCOMPAT_64BIT: u32 = 0x80;
/// Bytes at the start of a device that must be zero for it to count as blank.
const BLANK_PROBE: usize = 64 * 1024;

/// Sets up every mount, in configuration order. Stops at the first failure.
pub fn setup_all(tools: &Path, mounts: &[GuestMount]) -> Result<(), String> {
    for m in mounts {
        match m {
            GuestMount::Volume {
                device,
                target,
                size,
            } => setup_volume(tools, Path::new(device), Path::new(target), *size)
                .map_err(|e| format!("volume at {target}: {e}"))?,
            GuestMount::Bind {
                tag,
                target,
                read_only,
            } => mount_bind(tag, Path::new(target), *read_only)
                .map_err(|e| format!("bind mount {tag} at {target}: {e}"))?,
        }
    }
    Ok(())
}

/// Formats `device` if blank, mounts it at `target` and grows the filesystem
/// to `size` bytes when it is smaller (online, so no fsck is needed first).
pub fn setup_volume(tools: &Path, device: &Path, target: &Path, size: u64) -> Result<(), String> {
    crate::boot::wait_for_device(device, "volume").map_err(|e| e.to_string())?;
    if format_if_blank(tools, device)? {
        eprintln!("clankerd-guestd: formatted new volume {}", device.display());
    }
    fs::create_dir_all(target).map_err(|e| format!("creating {}: {e}", target.display()))?;
    crate::boot::mount(
        &device.to_string_lossy(),
        &target.to_string_lossy(),
        "ext4",
        libc::MS_NOATIME,
        "",
    )
    .map_err(|e| e.to_string())?;
    if grow_if_needed(tools, device, size)? {
        eprintln!("clankerd-guestd: grew volume to {size} bytes");
    }
    Ok(())
}

fn mount_bind(tag: &str, target: &Path, read_only: bool) -> io::Result<()> {
    fs::create_dir_all(target)?;
    let flags = if read_only { libc::MS_RDONLY } else { 0 };
    crate::boot::mount(tag, &target.to_string_lossy(), "virtiofs", flags, "")
}

/// Formats `device` when it holds nothing; returns whether it did. A device
/// that holds something other than ext4 is refused rather than overwritten.
pub fn format_if_blank(tools: &Path, device: &Path) -> Result<bool, String> {
    if ext4_bytes(device).map_err(|e| e.to_string())?.is_some() {
        return Ok(false);
    }
    if !is_blank(device).map_err(|e| e.to_string())? {
        return Err(format!(
            "{} holds data that is not an ext4 filesystem; refusing to format it",
            device.display()
        ));
    }
    let ctx = Ctx {
        boot_dir: tools.to_path_buf(),
        populate_mode: false,
        lenient: false,
    };
    format_device(&ctx, device, None, Some("volume")).map_err(|f| f.1)?;
    Ok(true)
}

/// Grows the filesystem on `device` to fill it when it is smaller than `want`
/// bytes; returns whether it did. Fails if the device itself is smaller.
pub fn grow_if_needed(tools: &Path, device: &Path, want: u64) -> Result<bool, String> {
    let Some(fs_bytes) = ext4_bytes(device).map_err(|e| e.to_string())? else {
        return Err(format!("{} has no ext4 filesystem", device.display()));
    };
    if fs_bytes >= want {
        return Ok(false);
    }
    let have = device_bytes(device).map_err(|e| e.to_string())?;
    if have < want {
        return Err(format!(
            "{} is {have} bytes, smaller than the requested {want}",
            device.display()
        ));
    }
    let resize2fs = tools.join("resize2fs");
    if !resize2fs.exists() {
        return Err(format!(
            "{} not found (the boot directory must ship resize2fs)",
            resize2fs.display()
        ));
    }
    let out = Command::new(&resize2fs)
        .arg(device)
        .output()
        .map_err(|e| format!("running {}: {e}", resize2fs.display()))?;
    if !out.status.success() {
        return Err(format!(
            "resize2fs failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(true)
}

/// Size in bytes of the ext4 filesystem on `device`, or `None` if there is no
/// ext4 superblock.
pub fn ext4_bytes(device: &Path) -> io::Result<Option<u64>> {
    let mut f = File::open(device)?;
    f.seek(SeekFrom::Start(SUPERBLOCK))?;
    let mut sb = [0u8; 1024];
    if f.read_exact(&mut sb).is_err() {
        return Ok(None);
    }
    let u16_at = |o: usize| u16::from_le_bytes(sb[o..o + 2].try_into().unwrap());
    let u32_at = |o: usize| u32::from_le_bytes(sb[o..o + 4].try_into().unwrap());
    if u16_at(56) != EXT4_MAGIC {
        return Ok(None);
    }
    let mut blocks = u64::from(u32_at(4));
    if u32_at(96) & INCOMPAT_64BIT != 0 {
        blocks |= u64::from(u32_at(0x150)) << 32;
    }
    Ok(Some(blocks * (1024u64 << u32_at(24))))
}

fn is_blank(device: &Path) -> io::Result<bool> {
    let mut buf = vec![0u8; BLANK_PROBE];
    let n = File::open(device)?.read(&mut buf)?;
    Ok(buf[..n].iter().all(|&b| b == 0))
}

/// Size of a regular file or block device (seeking to the end works for both).
pub fn device_bytes(device: &Path) -> io::Result<u64> {
    File::open(device)?.seek(SeekFrom::End(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    const MIB: u64 = 1 << 20;

    fn host_tool(name: &str) -> Option<PathBuf> {
        ["/usr/sbin", "/sbin", "/usr/bin"]
            .iter()
            .map(|d| Path::new(d).join(name))
            .find(|p| p.exists())
    }

    /// A tools directory linking the host's mke2fs and resize2fs (stand-ins
    /// for the static binaries the boot directory ships), or a skip notice.
    fn tools(test: &str) -> Option<tempfile::TempDir> {
        let dir = tempfile::tempdir().unwrap();
        for name in ["mke2fs", "resize2fs"] {
            let Some(src) = host_tool(name) else {
                eprintln!("\nSKIPPED {test}: {name} (e2fsprogs) is not installed");
                return None;
            };
            symlink(src, dir.path().join(name)).unwrap();
        }
        Some(dir)
    }

    fn sparse(dir: &Path, size: u64) -> PathBuf {
        let path = dir.join("vol.ext4");
        File::create(&path).unwrap().set_len(size).unwrap();
        path
    }

    fn set_len(path: &Path, size: u64) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(size)
            .unwrap();
    }

    #[test]
    fn a_blank_volume_file_is_formatted_once() {
        let Some(tools) = tools("a_blank_volume_file_is_formatted_once") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let vol = sparse(dir.path(), 16 * MIB);
        assert_eq!(ext4_bytes(&vol).unwrap(), None);

        assert!(format_if_blank(tools.path(), &vol).unwrap());
        assert_eq!(ext4_bytes(&vol).unwrap(), Some(16 * MIB));
        // A second start must not wipe it.
        assert!(!format_if_blank(tools.path(), &vol).unwrap());
    }

    #[test]
    fn a_device_holding_foreign_data_is_never_formatted() {
        let Some(tools) = tools("a_device_holding_foreign_data_is_never_formatted") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let vol = sparse(dir.path(), 4 * MIB);
        fs::write(&vol, vec![7u8; 4096]).unwrap();
        let err = format_if_blank(tools.path(), &vol).unwrap_err();
        assert!(err.contains("not an ext4"), "{err}");
        assert_eq!(fs::read(&vol).unwrap()[0], 7);
    }

    #[test]
    fn a_larger_device_grows_the_filesystem() {
        let Some(tools) = tools("a_larger_device_grows_the_filesystem") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let vol = sparse(dir.path(), 16 * MIB);
        format_if_blank(tools.path(), &vol).unwrap();

        // Same size: nothing to do.
        assert!(!grow_if_needed(tools.path(), &vol, 16 * MIB).unwrap());

        // The host enlarges the sparse file and passes the new size.
        set_len(&vol, 48 * MIB);
        assert!(grow_if_needed(tools.path(), &vol, 48 * MIB).unwrap());
        assert_eq!(ext4_bytes(&vol).unwrap(), Some(48 * MIB));
        assert!(!grow_if_needed(tools.path(), &vol, 48 * MIB).unwrap());
    }

    #[test]
    fn growing_beyond_the_device_is_an_error() {
        let Some(tools) = tools("growing_beyond_the_device_is_an_error") else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let vol = sparse(dir.path(), 16 * MIB);
        format_if_blank(tools.path(), &vol).unwrap();
        let err = grow_if_needed(tools.path(), &vol, 64 * MIB).unwrap_err();
        assert!(err.contains("smaller than the requested"), "{err}");
    }

    fn is_root() -> bool {
        // SAFETY: geteuid has no preconditions.
        unsafe { libc::geteuid() == 0 }
    }

    #[test]
    fn mounting_formats_grows_and_keeps_data_across_remounts() {
        let name = "mounting_formats_grows_and_keeps_data_across_remounts";
        let Some(tools) = tools(name) else { return };
        if !is_root() {
            eprintln!(
                "\nSKIPPED {name}: needs root for loop devices and mount (run: sudo -E cargo test -p clankerd-guestd --bin clankerd-guestd volume)"
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let vol = sparse(dir.path(), 16 * MIB);
        let target = dir.path().join("mnt");
        let umount = |t: &Path| {
            let c = std::ffi::CString::new(t.to_str().unwrap()).unwrap();
            // SAFETY: valid NUL-terminated path.
            assert_eq!(unsafe { libc::umount2(c.as_ptr(), 0) }, 0);
        };

        let loop1 = crate::rootdisk::LoopDevice::attach(&vol).unwrap();
        setup_volume(tools.path(), &loop1.path, &target, 16 * MIB).unwrap();
        fs::write(target.join("data"), b"kept").unwrap();
        umount(&target);
        drop(loop1);

        set_len(&vol, 48 * MIB);
        let loop2 = crate::rootdisk::LoopDevice::attach(&vol).unwrap();
        setup_volume(tools.path(), &loop2.path, &target, 48 * MIB).unwrap();
        assert_eq!(fs::read(target.join("data")).unwrap(), b"kept");
        assert_eq!(ext4_bytes(&loop2.path).unwrap(), Some(48 * MIB));
        umount(&target);
    }
}
