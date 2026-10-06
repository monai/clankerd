//! The systemd `LISTEN_FDS` convention: a parent hands already-bound listening
//! sockets to a child as fds 3.. and describes them in the environment.
//!
//! Host-side port listeners are bound by the library (so a failure to bind is
//! reported to the caller) and passed to clankerd-vmspawn this way, which lets
//! them outlive the library process.

use std::os::fd::{FromRawFd, OwnedFd, RawFd};

/// First fd of the passed sockets.
pub const FIRST_FD: RawFd = 3;

/// A passed socket: its fd and the name from `LISTEN_FDNAMES`
/// (`unknown` when unnamed).
#[derive(Debug)]
pub struct Passed {
    pub fd: OwnedFd,
    pub name: String,
}

/// Parses the convention's variables without touching any fd.
/// Returns `(names)` for each of the `LISTEN_FDS` sockets.
/// `pid` is the caller's pid; a `LISTEN_PID` for another process means the
/// variables were inherited by accident and are ignored.
pub fn parse(
    listen_pid: Option<&str>,
    listen_fds: Option<&str>,
    listen_fdnames: Option<&str>,
    pid: u32,
) -> Vec<String> {
    if let Some(p) = listen_pid
        && p.trim().parse::<u32>().ok() != Some(pid)
    {
        return Vec::new();
    }
    let count: usize = listen_fds.and_then(|v| v.trim().parse().ok()).unwrap_or(0);
    let mut names = listen_fdnames
        .map(|v| v.split(':').map(str::to_owned).collect::<Vec<_>>())
        .unwrap_or_default();
    names.resize(count, "unknown".to_owned());
    names
        .into_iter()
        .map(|n| if n.is_empty() { "unknown".into() } else { n })
        .collect()
}

/// Takes ownership of the passed sockets and clears the variables.
///
/// # Safety
/// Must be called at most once, early, and only if the parent really passed
/// the fds (the variables are trusted). The fds are marked close-on-exec.
pub unsafe fn take() -> Vec<Passed> {
    let var = |k| std::env::var(k).ok();
    let names = parse(
        var("LISTEN_PID").as_deref(),
        var("LISTEN_FDS").as_deref(),
        var("LISTEN_FDNAMES").as_deref(),
        std::process::id(),
    );
    // SAFETY: single-threaded early startup is the caller's obligation.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    names
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let fd = FIRST_FD + i as RawFd;
            // SAFETY: the convention says fds 3.. are ours.
            unsafe {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
                Passed {
                    fd: OwnedFd::from_raw_fd(fd),
                    name,
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_matched_to_fds() {
        assert_eq!(
            parse(Some("7"), Some("2"), Some("web:api"), 7),
            ["web", "api"]
        );
    }

    #[test]
    fn missing_names_are_unknown() {
        assert_eq!(parse(None, Some("2"), None, 7), ["unknown", "unknown"]);
    }

    #[test]
    fn variables_for_another_process_are_ignored() {
        assert!(parse(Some("8"), Some("1"), None, 7).is_empty());
        assert!(parse(None, None, None, 7).is_empty());
    }
}
