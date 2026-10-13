use std::fs::{self, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{NetAttachment, NetBackend, NetSpec};
use crate::error::{Error, Result};

const PID_FILE: &str = "net.pid";
const LOG_FILE: &str = "vmnet-helper.log";
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_secs(3);

/// Native vmnet sidecar; installed separately using vmnet-helper's installer.
pub struct VmnetBackend {
    binary: PathBuf,
}
impl VmnetBackend {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }
}
impl Default for VmnetBackend {
    fn default() -> Self {
        Self::new("/opt/vmnet-helper/bin/vmnet-helper")
    }
}

impl NetBackend for VmnetBackend {
    fn start(&self, spec: &NetSpec) -> Result<NetAttachment> {
        let binary = &self.binary;
        // A leftover sidecar of an earlier run would hold the socket.
        self.stop(spec);
        let log_path = spec.dir.join(LOG_FILE);
        let log = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)?;
        let mut command = if cfg!(target_os = "macos") {
            let mut command = Command::new("sudo");
            command.args(["--non-interactive", "--"]).arg(binary);
            command
        } else {
            Command::new(binary)
        };
        let identity = interface_identity(&spec.machine_id);
        let mut child = command
            .arg("--socket")
            .arg(&spec.socket)
            .args([
                "--operation-mode",
                "shared",
                "--interface-id",
                &identity,
                "--start-address",
                "192.168.105.1",
                "--end-address",
                "192.168.105.254",
                "--subnet-mask",
                "255.255.255.0",
            ])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()
            .map_err(|e| {
                Error::unavailable(format!(
                    "cannot start vmnet-helper ({}): {e}",
                    binary.display()
                ))
            })?;
        if let Err(error) = fs::write(spec.dir.join(PID_FILE), child.id().to_string()) {
            signal_group(child.id() as i32, libc::SIGTERM);
            let _ = child.wait();
            let _ = fs::remove_file(&spec.socket);
            return Err(error.into());
        }

        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            let failure = match child.try_wait() {
                Ok(Some(status)) => Some(format!(
                    "vmnet-helper exited ({status}) before it was ready"
                )),
                Ok(None) if Instant::now() >= deadline => {
                    Some(format!("vmnet-helper not ready after {READY_TIMEOUT:?}"))
                }
                Ok(None) => None,
                Err(e) => Some(format!("waiting for vmnet-helper: {e}")),
            };
            if let Some(failure) = failure {
                self.stop(spec);
                let _ = child.wait();
                return Err(Error::unavailable(format!(
                    "{failure}: {}",
                    log_tail(&log_path)
                )));
            }
            if spec.socket.exists() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // sudo may keep a privileged monitor in a separate process group.
        // Record the helper after it drops privileges, so later CLI processes
        // can terminate it directly without sudo or a live parent.
        if cfg!(target_os = "macos") {
            let pid = helper_pid(&spec.socket).ok_or_else(|| {
                self.stop(spec);
                Error::unavailable("cannot identify the unprivileged vmnet-helper process")
            })?;
            fs::write(spec.dir.join(PID_FILE), pid.to_string())?;
        }
        // Reap it when it ends while we are still alive.
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        Ok(NetAttachment {
            socket: spec.socket.clone(),
            mac: machine_mac(&spec.machine_id),
            features: 0,
            vfkit: false,
        })
    }

    fn stop(&self, spec: &NetSpec) {
        // A failed sudo startup may leave the monitor PID in the record.
        // Signal the actual dropped-privilege helper as well, if it exists.
        if cfg!(target_os = "macos")
            && let Some(pid) = helper_pid(&spec.socket)
        {
            signal_group(pid, libc::SIGTERM);
        }
        let pid_file = spec.dir.join(PID_FILE);
        if let Some(pid) = fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            && is_helper(pid, &spec.socket)
        {
            signal_group(pid, libc::SIGTERM);
            let deadline = Instant::now() + STOP_GRACE;
            while process_exists(pid) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            if process_exists(pid) {
                signal_group(pid, libc::SIGKILL);
            }
        }
        let _ = fs::remove_file(pid_file);
        let _ = fs::remove_file(&spec.socket);
    }
}

fn signal_group(pid: i32, signal: i32) {
    // SAFETY: plain signal delivery; a stale pid at worst fails with ESRCH.
    unsafe {
        libc::kill(-pid, signal);
        libc::kill(pid, signal);
    }
}

fn process_exists(pid: i32) -> bool {
    // SAFETY: signal 0 only probes.
    unsafe {
        libc::kill(pid, 0) == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/// Guards against a stale pid file (after a host reboot the pid may belong to
/// anything): only signal a process whose command line mentions vmnet-helper. If
/// `ps` cannot be run the pid file is trusted.
fn is_helper(pid: i32, socket: &Path) -> bool {
    if !process_exists(pid) {
        return false;
    }
    match Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
    {
        Ok(out) if out.status.success() || !out.stdout.is_empty() => {
            let command = String::from_utf8_lossy(&out.stdout);
            command.contains("vmnet-helper") && command.contains(socket.to_string_lossy().as_ref())
        }
        Ok(_) => false,
        Err(_) => false,
    }
}

fn log_tail(path: &Path) -> String {
    let log = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(5)..];
    if tail.is_empty() {
        "it printed nothing (see vmnet-helper.log in the machine directory)".into()
    } else {
        tail.join(" | ")
    }
}

fn identity_bytes(id: &str) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    Sha256::digest(id.as_bytes()).into()
}
fn machine_mac(id: &str) -> [u8; 6] {
    let hash = identity_bytes(id);
    [0x02, hash[1], hash[2], hash[3], hash[4], hash[5]]
}
fn interface_identity(id: &str) -> String {
    let h = identity_bytes(id);
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-4{:01x}{:02x}-8{:01x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        h[0],
        h[1],
        h[2],
        h[3],
        h[4],
        h[5],
        h[6] & 15,
        h[7],
        h[8] & 15,
        h[9],
        h[10],
        h[11],
        h[12],
        h[13],
        h[14],
        h[15]
    )
}

fn helper_pid(socket: &Path) -> Option<i32> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,uid=,command="])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let pid = fields.next()?.parse().ok()?;
        let uid: u32 = fields.next()?.parse().ok()?;
        let program = fields.next()?;
        // SAFETY: geteuid simply reads the process identity.
        if uid != unsafe { libc::geteuid() } || !program.ends_with("/vmnet-helper") {
            return None;
        }
        let args: Vec<_> = fields.collect();
        if args
            .windows(2)
            .any(|pair| pair[0] == "--socket" && pair[1] == socket.to_string_lossy())
        {
            Some(pid)
        } else {
            None
        }
    })
}
