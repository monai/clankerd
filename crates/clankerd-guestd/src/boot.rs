//! Guest boot: guestd remains in the packaged management root.
//! The OCI disk and its configured mounts are prepared under /workload;
//! workload_root owns the separate namespace and child-only root switch.

use std::ffi::CString;
use std::fs;
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use clankerd_proto::guest::{Clock, Workload};
use clankerd_proto::spawn::ROOT_DEVICE;

use crate::poll::poll_until;

/// Marker file image authors test for (like Docker's `/.dockerenv`).
pub const MARKER: &str = ".clankerdenv";
const DEVICE_WAIT: Duration = Duration::from_secs(10);

/// Initializes management services and the independent workload namespace.
pub fn init(workload: &Workload) -> Result<(), String> {
    mount_pseudo_filesystems().map_err(|e| format!("management mounts: {e}"))?;
    mount("", "/", "", libc::MS_REC | libc::MS_PRIVATE, "").map_err(|e| e.to_string())?;
    wait_for_device(Path::new(ROOT_DEVICE), "workload root").map_err(|e| e.to_string())?;
    fs::create_dir_all("/workload").map_err(|e| e.to_string())?;
    mount(ROOT_DEVICE, "/workload", "ext4", libc::MS_NOATIME, "").map_err(|e| e.to_string())?;
    fs::create_dir_all("/run/clankerd-resolver/etc").map_err(|e| e.to_string())?;
    let resolver = Path::new("/etc/resolv.conf");
    if fs::symlink_metadata(resolver).is_ok() {
        fs::remove_file(resolver).map_err(|e| e.to_string())?;
    }
    std::os::unix::fs::symlink("/run/clankerd-resolver/etc/resolv.conf", resolver)
        .map_err(|e| e.to_string())?;
    // The rest is best effort: a failure is logged, the workload still runs.
    match delegate_controllers(Path::new("/sys/fs/cgroup")) {
        Ok(enabled) if enabled.is_empty() => {
            eprintln!("clankerd-guestd: no cgroup v2 controllers available to delegate")
        }
        Ok(_) => {}
        Err(e) => eprintln!("clankerd-guestd: delegating cgroup controllers: {e}"),
    }
    // Forwarding sysctls always; the NIC only when the machine has one. A
    // machine whose network cannot be brought up fails before workload launch.
    crate::network::prepare();
    if let Some(net) = &workload.network
        && let Err(e) = crate::network::setup(net)
    {
        return Err(format!("network setup failed: {e}"));
    }
    if let Some(clock) = &workload.clock
        && let Err(e) = set_clock(clock)
    {
        eprintln!("clankerd-guestd: setting the clock: {e}");
    }
    write_marker(Path::new("/workload")).map_err(|e| format!("writing /{MARKER}: {e}"))?;
    // Volume and bind mounts; a failure here must not hand the workload a
    // missing data directory, so it fails the boot.
    crate::volume::setup_all(Path::new("/"), &workload.mounts)?;
    fs::create_dir_all("/run/clankerd-resolver").map_err(|e| e.to_string())?;
    if !Path::new("/run/clankerd-resolver/etc/resolv.conf").exists() {
        fs::write("/run/clankerd-resolver/etc/resolv.conf", "").map_err(|e| e.to_string())?;
    }
    crate::workload_root::start()?;
    crate::power::start_reaper();
    Ok(())
}

/// Writes the `/.clankerdenv` marker under `root`.
pub fn write_marker(root: &Path) -> io::Result<()> {
    fs::write(root.join(MARKER), b"")
}

/// Enables every available cgroup v2 controller for children of the cgroup at
/// `root` (writes `+name` entries to `cgroup.subtree_control`), so that
/// workloads such as dockerd can manage their own limits. Returns the
/// controllers enabled.
pub fn delegate_controllers(root: &Path) -> io::Result<Vec<String>> {
    let available = fs::read_to_string(root.join("cgroup.controllers"))?;
    let mut enabled = Vec::new();
    for name in available.split_whitespace() {
        // One write per controller: one that cannot be enabled (it is not
        // delegatable here) must not stop the others.
        let write = fs::write(root.join("cgroup.subtree_control"), format!("+{name}"));
        match write {
            Ok(()) => enabled.push(name.to_owned()),
            Err(e) => eprintln!("clankerd-guestd: cannot enable cgroup controller {name}: {e}"),
        }
    }
    Ok(enabled)
}

/// Sets the wall clock to the host's time.
pub fn set_clock(clock: &Clock) -> io::Result<()> {
    let ts = libc::timespec {
        tv_sec: clock.secs as _,
        tv_nsec: clock.nanos as _,
    };
    // SAFETY: clock_settime reads one timespec from a valid pointer.
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(crate) fn mount(
    src: &str,
    target: &str,
    fstype: &str,
    flags: libc::c_ulong,
    data: &str,
) -> io::Result<()> {
    let (src_c, target_c, fstype_c, data_c) = (
        CString::new(src)?,
        CString::new(target)?,
        CString::new(fstype)?,
        CString::new(data)?,
    );
    // SAFETY: valid NUL-terminated strings.
    let rc = unsafe {
        libc::mount(
            src_c.as_ptr(),
            target_c.as_ptr(),
            fstype_c.as_ptr(),
            flags,
            data_c.as_ptr().cast(),
        )
    };
    if rc < 0 {
        let e = io::Error::last_os_error();
        let kernel = kernel_log_tail()
            .map(|log| format!("\nkernel log:\n{log}"))
            .unwrap_or_default();
        return Err(io::Error::new(
            e.kind(),
            format!("mount {fstype} {src} on {target}: {e}{kernel}"),
        ));
    }
    Ok(())
}

fn kernel_log_tail() -> Option<String> {
    let mut buffer = vec![0u8; 64 * 1024];
    // SAFETY: SYSLOG_ACTION_READ_ALL reads non-destructively into the allocated buffer.
    let count = unsafe { libc::klogctl(3, buffer.as_mut_ptr().cast(), buffer.len() as i32) };
    if count <= 0 {
        return None;
    }
    let text = String::from_utf8_lossy(&buffer[..count as usize]);
    let lines: Vec<_> = text.lines().collect();
    Some(lines[lines.len().saturating_sub(16)..].join("\n"))
}

/// virtio-blk devices appear a moment after boot; `/dev` may also still need
/// mounting if libkrun's init has not.
/// Waits for a block device node to appear, mounting devtmpfs on /dev if the
/// kernel has not by the time half the wait is gone. `what` names the disk in
/// the error.
pub fn wait_for_device(device: &Path, what: &str) -> io::Result<()> {
    let half = Instant::now() + DEVICE_WAIT / 2;
    let mut tried_devtmpfs = false;
    let found = poll_until(DEVICE_WAIT, Duration::from_millis(50), || {
        if !tried_devtmpfs && !device.exists() && Instant::now() >= half {
            tried_devtmpfs = true;
            let _ = mount("devtmpfs", "/dev", "devtmpfs", 0, "mode=0755");
        }
        device.exists()
    });
    if found {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "{} did not appear (is the {what} attached?)",
                device.display()
            ),
        ))
    }
}

/// Mounts what a Linux userspace expects, over the image's (usually empty)
/// mount point directories.
fn mount_pseudo_filesystems() -> io::Result<()> {
    let nodev = libc::MS_NOSUID | libc::MS_NODEV;
    let table: [(&str, &str, &str, libc::c_ulong, &str); 8] = [
        ("tmpfs", "/run", "tmpfs", nodev, "mode=0755"),
        ("tmpfs", "/tmp", "tmpfs", nodev, "mode=1777"),
        ("proc", "/proc", "proc", nodev | libc::MS_NOEXEC, ""),
        ("sysfs", "/sys", "sysfs", nodev | libc::MS_NOEXEC, ""),
        ("devtmpfs", "/dev", "devtmpfs", libc::MS_NOSUID, "mode=0755"),
        (
            "devpts",
            "/dev/pts",
            "devpts",
            libc::MS_NOSUID | libc::MS_NOEXEC,
            "mode=0620,ptmxmode=0666",
        ),
        ("tmpfs", "/dev/shm", "tmpfs", nodev, "mode=1777"),
        (
            "cgroup2",
            "/sys/fs/cgroup",
            "cgroup2",
            nodev | libc::MS_NOEXEC,
            "",
        ),
    ];
    for (src, target, fstype, flags, data) in table {
        fs::create_dir_all(target)?;
        let inherited = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
        if pseudo_mount_present(&inherited, target, fstype)? {
            eprintln!("clankerd-guestd: reusing inherited {fstype} mount at {target}");
        } else {
            mount(src, target, fstype, flags, data).map_err(|error| {
                io::Error::new(error.kind(), format!("{error}\nmountinfo:\n{inherited}"))
            })?;
        }
    }
    for (link, dest) in [
        ("/dev/ptmx", "pts/ptmx"),
        ("/dev/fd", "/proc/self/fd"),
        ("/dev/stdin", "/proc/self/fd/0"),
        ("/dev/stdout", "/proc/self/fd/1"),
        ("/dev/stderr", "/proc/self/fd/2"),
    ] {
        let _ = fs::remove_file(link);
        std::os::unix::fs::symlink(dest, link)?;
    }
    Ok(())
}

fn pseudo_mount_present(mountinfo: &str, target: &str, expected: &str) -> io::Result<bool> {
    for line in mountinfo.lines().rev() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        if left.split_whitespace().nth(4) != Some(target) {
            continue;
        }
        let actual = right.split_whitespace().next().unwrap_or("");
        if actual != expected {
            return Err(io::Error::other(format!(
                "inherited mount at {target} is {actual}, expected {expected}: {line}"
            )));
        }
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inherited_pseudo_mounts_are_reused_only_at_the_expected_type_and_path() {
        let info = "20 1 0:3 / /sys rw,nosuid,nodev,noexec - sysfs sysfs rw\n21 1 0:4 / /dev rw - devtmpfs devtmpfs rw\n22 20 0:5 / /sys/fs/cgroup rw - cgroup2 cgroup2 rw\n";
        assert!(pseudo_mount_present(info, "/sys", "sysfs").unwrap());
        assert!(pseudo_mount_present(info, "/dev", "devtmpfs").unwrap());
        assert!(pseudo_mount_present(info, "/sys/fs/cgroup", "cgroup2").unwrap());
        assert!(!pseudo_mount_present(info, "/proc", "proc").unwrap());
        assert!(pseudo_mount_present(info, "/sys", "tmpfs").is_err());
        assert!(!pseudo_mount_present(info, "/sys/fs", "sysfs").unwrap());
    }

    #[test]
    fn every_available_controller_is_enabled_for_children() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("cgroup.controllers"),
            "cpuset cpu io memory pids\n",
        )
        .unwrap();
        fs::write(dir.path().join("cgroup.subtree_control"), "").unwrap();
        let enabled = delegate_controllers(dir.path()).unwrap();
        assert_eq!(enabled, ["cpuset", "cpu", "io", "memory", "pids"]);
        // A real cgroup file accumulates the `+name` writes; the stand-in
        // shows the last one.
        assert_eq!(
            fs::read_to_string(dir.path().join("cgroup.subtree_control")).unwrap(),
            "+pids"
        );
    }

    #[test]
    fn a_cgroup_root_without_controllers_enables_nothing() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("cgroup.controllers"), "\n").unwrap();
        assert!(delegate_controllers(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn a_missing_cgroup_root_is_an_error_not_a_panic() {
        assert!(delegate_controllers(Path::new("/nonexistent/cgroup")).is_err());
    }

    #[test]
    fn the_marker_is_an_empty_file_at_the_root() {
        let dir = tempfile::tempdir().unwrap();
        write_marker(dir.path()).unwrap();
        let meta = fs::metadata(dir.path().join(".clankerdenv")).unwrap();
        assert!(meta.is_file());
        assert_eq!(meta.len(), 0);
    }
}
