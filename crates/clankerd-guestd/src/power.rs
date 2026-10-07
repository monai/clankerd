//! PID 1 duties: reaping orphans and ending the machine (stop processes, sync,
//! unmount, power off).
//!
//! guestd spawns the workload and exec sessions itself and waits for them by
//! pid, so the reaper must leave those alone. Spawning therefore goes through
//! [`spawn_owned`], which registers the child while holding the lock the
//! reaper takes; whatever else becomes a zombie was adopted from a dead
//! parent and is reaped here.

use std::collections::HashSet;
use std::process::{Child, Command};
use std::sync::Mutex;
use std::time::Duration;

/// Taken while spawning and while reaping, so a child is registered before
/// the reaper can see it as a zombie.
static OWNED: Mutex<Option<HashSet<i32>>> = Mutex::new(None);

fn owned() -> std::sync::MutexGuard<'static, Option<HashSet<i32>>> {
    OWNED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Spawns `cmd` and registers the child as one its owner will wait for.
pub fn spawn_owned(cmd: &mut Command) -> std::io::Result<Child> {
    let mut guard = owned();
    let child = cmd.spawn()?;
    guard
        .get_or_insert_with(HashSet::new)
        .insert(child.id() as i32);
    Ok(child)
}

/// Call after the owner has waited for `pid`.
pub fn release(pid: u32) {
    if let Some(set) = owned().as_mut() {
        set.remove(&(pid as i32));
    }
}

/// Reaps orphans adopted by PID 1, forever. Only meaningful as PID 1.
pub fn start_reaper() {
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_millis(100));
            loop {
                let guard = owned();
                // SAFETY: waitid fills the zeroed siginfo we pass; WNOWAIT only peeks.
                let pid = unsafe {
                    let mut info: libc::siginfo_t = std::mem::zeroed();
                    let rc = libc::waitid(
                        libc::P_ALL,
                        0,
                        &mut info,
                        libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                    );
                    if rc < 0 { 0 } else { info.si_pid() }
                };
                if pid == 0 {
                    break;
                }
                let ours = guard.as_ref().is_some_and(|s| s.contains(&pid));
                if ours {
                    // Its owner is about to wait for it; look again shortly.
                    break;
                }
                // SAFETY: reaping a zombie nobody else waits for.
                unsafe { libc::waitpid(pid, std::ptr::null_mut(), libc::WNOHANG) };
            }
        }
    });
}

/// Ends the machine once the workload is done: as PID 1 it stops every
/// process, syncs, unmounts and powers off; elsewhere (the Linux stand-in) it
/// just exits.
pub fn exit_machine() -> ! {
    if std::process::id() == 1 {
        teardown();
        power_off();
    }
    std::process::exit(0);
}

/// Flushes and powers the VM off; as PID 1 returning would panic the kernel.
pub fn power_off() -> ! {
    // SAFETY: sync and reboot take no pointers.
    unsafe {
        libc::sync();
        libc::reboot(libc::RB_POWER_OFF);
    }
    // reboot only returns on failure; PID 1 must not exit.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

/// SIGTERM everything, wait, SIGKILL what is left, then sync and unmount.
fn teardown() {
    // Progress goes to the console (`vmctl logs`), the only trace of a shutdown.
    eprintln!("clankerd-guestd: stopping processes");
    signal_all(libc::SIGTERM);
    if !wait_for_userspace_to_end(Duration::from_secs(3)) {
        eprintln!("clankerd-guestd: killing processes that ignored SIGTERM");
        signal_all(libc::SIGKILL);
        wait_for_userspace_to_end(Duration::from_secs(2));
    }
    eprintln!("clankerd-guestd: syncing and unmounting");
    // SAFETY: sync has no preconditions.
    unsafe { libc::sync() };
    let mounts = std::fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    for target in unmountable(&mounts) {
        let Ok(path) = std::ffi::CString::new(target) else {
            continue;
        };
        // SAFETY: valid NUL-terminated path.
        unsafe {
            if libc::umount2(path.as_ptr(), 0) < 0 {
                eprintln!(
                    "clankerd-guestd: {} is busy, detaching it",
                    path.to_string_lossy()
                );
                libc::umount2(path.as_ptr(), libc::MNT_DETACH);
            }
        }
    }
    // The root cannot be unmounted from inside; a read-only remount flushes it
    // and leaves the filesystem clean.
    // SAFETY: valid NUL-terminated strings; no data argument.
    unsafe {
        libc::mount(
            std::ptr::null(),
            c"/".as_ptr(),
            std::ptr::null(),
            libc::MS_REMOUNT | libc::MS_RDONLY,
            std::ptr::null(),
        );
        libc::sync();
    }
    eprintln!("clankerd-guestd: powering off");
}

fn signal_all(signal: i32) {
    // SAFETY: -1 signals every process except init; that is the point.
    unsafe { libc::kill(-1, signal) };
}

/// Waits (reaping) until no userspace process but us is left.
fn wait_for_userspace_to_end(limit: Duration) -> bool {
    crate::poll::poll_until(limit, Duration::from_millis(50), || {
        // SAFETY: reaping children; no pointers.
        while unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) } > 0 {}
        userspace_pids().is_empty()
    })
}

/// Live userspace processes other than PID 1: kernel threads have no command
/// line, zombies are only waiting to be reaped.
fn userspace_pids() -> Vec<i32> {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    dir.flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<i32>().ok())
        .filter(|&pid| pid > 1)
        .filter(|pid| {
            let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            let zombie = stat
                .rsplit(')')
                .next()
                .is_some_and(|s| s.trim_start().starts_with('Z'));
            !cmdline.is_empty() && !zombie
        })
        .collect()
}

/// Mount points to unmount at shutdown from the contents of `/proc/self/mounts`:
/// disk and shared-directory filesystems, newest first, never the root.
pub fn unmountable(mounts: &str) -> Vec<String> {
    const FILESYSTEMS: [&str; 7] = ["ext4", "ext3", "ext2", "xfs", "btrfs", "vfat", "virtiofs"];
    let mut targets: Vec<String> = mounts
        .lines()
        .filter_map(|l| {
            let mut f = l.split_whitespace();
            let (_src, target, fstype) = (f.next()?, f.next()?, f.next()?);
            (FILESYSTEMS.contains(&fstype) && target != "/").then(|| unescape(target))
        })
        .collect();
    targets.reverse();
    targets
}

/// `/proc/self/mounts` escapes space, tab, newline and backslash as octal.
fn unescape(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('\\') {
        out.push_str(&rest[..i]);
        let code = rest
            .get(i + 1..i + 4)
            .and_then(|o| u8::from_str_radix(o, 8).ok());
        match code {
            Some(c) => {
                out.push(c as char);
                rest = &rest[i + 4..];
            }
            None => {
                out.push('\\');
                rest = &rest[i + 1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_and_shared_filesystems_are_unmounted_newest_first_and_never_the_root() {
        let mounts = "\
/dev/vda / ext4 rw,relatime 0 0
proc /proc proc rw 0 0
tmpfs /run tmpfs rw 0 0
/dev/vdb /storage ext4 rw 0 0
share /workspace virtiofs rw 0 0
/dev/vda /run/clankerd/boot virtiofs ro 0 0
/dev/vdc /with\\040space ext4 rw 0 0
";
        assert_eq!(
            unmountable(mounts),
            [
                "/with space",
                "/run/clankerd/boot",
                "/workspace",
                "/storage"
            ]
        );
    }

    /// This process stands in for PID 1: as a subreaper it adopts orphans.
    #[test]
    fn adopted_orphans_are_reaped_while_owned_children_keep_their_status() {
        // SAFETY: prctl with PR_SET_CHILD_SUBREAPER takes plain integers.
        assert_eq!(
            unsafe { libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) },
            0
        );
        start_reaper();

        // An owned child exits with its own status, not stolen by the reaper.
        let mut owned = spawn_owned(Command::new("sh").args(["-c", "exit 3"])).unwrap();
        let status = owned.wait().unwrap();
        release(owned.id());
        assert_eq!(status.code(), Some(3));

        // A grandchild outliving its parent is adopted by us and must not linger as a zombie.
        let mut parent =
            spawn_owned(Command::new("sh").args(["-c", "sleep 0.2 & exit 0"])).unwrap();
        parent.wait().unwrap();
        release(parent.id());
        // The grandchild exits after 0.2 s; the reaper polls every 0.1 s.
        std::thread::sleep(Duration::from_secs(1));
        // SAFETY: peeks at a zombie child without reaping it.
        let (rc, zombie) = unsafe {
            let mut info: libc::siginfo_t = std::mem::zeroed();
            let rc = libc::waitid(
                libc::P_ALL,
                0,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            );
            (rc, info.si_pid())
        };
        // rc < 0 is ECHILD (no children at all); otherwise no zombie may be waiting.
        assert!(rc < 0 || zombie == 0, "zombie {zombie} was not reaped");
    }
}
