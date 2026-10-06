//! Guest init when guestd is PID 1 on a root disk (`--boot`): mount the root
//! disk and pivot into it, mount the pseudo filesystems, delegate cgroup v2
//! controllers, set the clock from the host and write the `/.clankerdenv`
//! marker.
//!
//! The Linux stand-in used by tests skips all of this: it needs root and a
//! block device, and would change the host. The parts with no such need
//! ([`delegate_controllers`], [`write_marker`]) take a root path and are unit
//! tested; the mount and pivot sequence is verified on the M2 only.

use std::ffi::CString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::{Duration, Instant};

use clankerd_proto::guest::{Clock, Workload};
use clankerd_proto::spawn::{GUEST_BOOT_MOUNT, ROOT_DEVICE};

/// Marker file image authors test for (like Docker's `/.dockerenv`).
pub const MARKER: &str = ".clankerdenv";
const NEW_ROOT: &str = "/newroot";
const DEVICE_WAIT: Duration = Duration::from_secs(10);

/// Runs the whole boot sequence. After it returns, `/` is the image's root.
pub fn init(workload: &Workload) -> Result<(), String> {
    mount_root_and_pivot().map_err(|e| format!("switching to the root disk: {e}"))?;
    mount_pseudo_filesystems().map_err(|e| format!("mounting /proc, /sys and /dev: {e}"))?;
    // The rest is best effort: a failure is logged, the workload still runs.
    match delegate_controllers(Path::new("/sys/fs/cgroup")) {
        Ok(enabled) if enabled.is_empty() => {
            eprintln!("clankerd-guestd: no cgroup v2 controllers available to delegate")
        }
        Ok(_) => {}
        Err(e) => eprintln!("clankerd-guestd: delegating cgroup controllers: {e}"),
    }
    if let Some(clock) = &workload.clock
        && let Err(e) = set_clock(clock)
    {
        eprintln!("clankerd-guestd: setting the clock: {e}");
    }
    write_marker(Path::new("/")).map_err(|e| format!("writing /{MARKER}: {e}"))?;
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
        tv_sec: clock.secs as libc::time_t,
        tv_nsec: clock.nanos as _,
    };
    // SAFETY: clock_settime reads one timespec from a valid pointer.
    if unsafe { libc::clock_settime(libc::CLOCK_REALTIME, &ts) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn cstr(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(io::Error::other)
}

fn mount(
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
        return Err(io::Error::new(
            e.kind(),
            format!("mount {fstype} {src} on {target}: {e}"),
        ));
    }
    Ok(())
}

/// Mounts the root disk, keeps the boot directory reachable at
/// [`GUEST_BOOT_MOUNT`] (read-only) and pivots into the disk.
fn mount_root_and_pivot() -> io::Result<()> {
    wait_for_device(Path::new(ROOT_DEVICE))?;
    fs::create_dir_all(NEW_ROOT)?;
    mount(ROOT_DEVICE, NEW_ROOT, "ext4", libc::MS_NOATIME, "")?;

    let run = format!("{NEW_ROOT}/run");
    fs::create_dir_all(&run)?;
    mount(
        "tmpfs",
        &run,
        "tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV,
        "mode=0755",
    )?;
    let boot = format!("{NEW_ROOT}{GUEST_BOOT_MOUNT}");
    let old = format!("{NEW_ROOT}/run/clankerd/old");
    fs::create_dir_all(&boot)?;
    fs::create_dir_all(&old)?;
    // pivot_root refuses shared mounts.
    mount("", "/", "", libc::MS_REC | libc::MS_PRIVATE, "")?;
    mount("/", &boot, "", libc::MS_BIND, "")?;
    mount(
        "",
        &boot,
        "",
        libc::MS_BIND | libc::MS_REMOUNT | libc::MS_RDONLY,
        "",
    )?;

    let (new_c, old_c) = (cstr(Path::new(NEW_ROOT))?, cstr(Path::new(&old))?);
    // SAFETY: pivot_root(2) takes two NUL-terminated paths.
    if unsafe { libc::syscall(libc::SYS_pivot_root, new_c.as_ptr(), old_c.as_ptr()) } < 0 {
        return Err(io::Error::other(format!(
            "pivot_root: {}",
            io::Error::last_os_error()
        )));
    }
    std::env::set_current_dir("/")?;
    let old_in_new = c"/run/clankerd/old";
    // SAFETY: valid NUL-terminated path. The old root stays reachable through
    // the bind mount above; guestd's own binary lives there, which the detached
    // mount keeps alive until it exits.
    if unsafe { libc::umount2(old_in_new.as_ptr(), libc::MNT_DETACH) } < 0 {
        return Err(io::Error::other(format!(
            "detaching the old root: {}",
            io::Error::last_os_error()
        )));
    }
    let _ = fs::remove_dir("/run/clankerd/old");
    Ok(())
}

/// virtio-blk devices appear a moment after boot; `/dev` may also still need
/// mounting if libkrun's init has not.
fn wait_for_device(device: &Path) -> io::Result<()> {
    let deadline = Instant::now() + DEVICE_WAIT;
    let mut tried_devtmpfs = false;
    while !device.exists() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "{} did not appear (is the root disk attached?)",
                    device.display()
                ),
            ));
        }
        if !tried_devtmpfs && deadline.saturating_duration_since(Instant::now()) < DEVICE_WAIT / 2 {
            tried_devtmpfs = true;
            let _ = mount("devtmpfs", "/dev", "devtmpfs", 0, "mode=0755");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(())
}

/// Mounts what a Linux userspace expects, over the image's (usually empty)
/// mount point directories.
fn mount_pseudo_filesystems() -> io::Result<()> {
    let nodev = libc::MS_NOSUID | libc::MS_NODEV;
    let table: [(&str, &str, &str, libc::c_ulong, &str); 6] = [
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
        mount(src, target, fstype, flags, data)?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
