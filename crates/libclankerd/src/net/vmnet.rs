use std::fs::{self, OpenOptions};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use super::{NetAttachment, NetBackend, NetSpec};
use crate::NetworkMode;
use crate::error::{Error, Result};

const PID_FILE: &str = "net.pid";
const LOG_FILE: &str = "clankerd-network.log";
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_secs(3);

/// Bundled clankerd-owned native vmnet sidecar.
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
        let executable = std::env::current_exe().unwrap_or_default();
        Self::new(
            executable
                .parent()
                .unwrap_or(Path::new("."))
                .join("clankerd-network"),
        )
    }
}

impl NetBackend for VmnetBackend {
    fn start(&self, spec: &NetSpec) -> Result<NetAttachment> {
        if spec.host_config.network_mode == NetworkMode::Shared
            && spec.host_config.shared_ipv6_prefix.is_none()
        {
            return Err(Error::invalid_parameter(
                "shared attachment requires its virtual network's persisted IPv6 prefix",
            ));
        }
        crate::config::validate_bridge_config(&spec.host_config)?;
        super::validate_host_config(&spec.host_config)?;
        let binary = &self.binary;
        // Interactive authorization is a separate step; a person entering
        // their password must not consume the native startup timeout.
        if cfg!(target_os = "macos") {
            let status = Command::new(binary).arg("authorize").status()?;
            if !status.success() {
                return Err(Error::unavailable("native networking authorization failed"));
            }
        }
        // A leftover sidecar of an earlier run would hold the socket.
        self.stop_helper(spec);
        if let NetworkMode::Bridged = spec.host_config.network_mode {
            super::bridge::validate_interface(
                binary,
                spec.host_config.bridge_interface.as_deref().unwrap(),
            )?;
        }
        let log_path = spec.dir.join(LOG_FILE);
        let log = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)?;
        let mut command = Command::new(binary);
        if cfg!(target_os = "macos") {
            command.arg("launch");
        }
        let identity = interface_identity(&spec.machine_id);
        command
            .arg("--socket")
            .arg(&spec.socket)
            .args(["--interface-id", &identity]);
        match spec.host_config.network_mode {
            NetworkMode::Shared | NetworkMode::HostOnly => {
                let (start, end, mask) = spec.host_config.shared_network.addresses()?;
                command.args([
                    "--operation-mode",
                    if spec.host_config.network_mode == NetworkMode::HostOnly {
                        "host"
                    } else {
                        "shared"
                    },
                    "--start-address",
                    &start.to_string(),
                    "--end-address",
                    &end.to_string(),
                    "--subnet-mask",
                    &mask.to_string(),
                ]);
                if let Some(prefix) = spec.host_config.shared_ipv6_prefix {
                    command.args(["--nat66-prefix", &prefix.to_string()]);
                }
            }
            NetworkMode::Bridged => {
                command.args([
                    "--operation-mode",
                    "bridged",
                    "--shared-interface",
                    spec.host_config.bridge_interface.as_deref().unwrap(),
                ]);
            }
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()
            .map_err(|e| {
                Error::unavailable(format!(
                    "cannot start clankerd-network ({}): {e}",
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
                    "clankerd-network exited ({status}) before it was ready"
                )),
                Ok(None) if Instant::now() >= deadline => Some(format!(
                    "clankerd-network not ready after {READY_TIMEOUT:?} (waiting for native interface metadata and socket)"
                )),
                Ok(None) => None,
                Err(e) => Some(format!("waiting for clankerd-network: {e}")),
            };
            if let Some(failure) = failure {
                self.stop_helper(spec);
                let _ = child.wait();
                return Err(Error::unavailable(format!(
                    "{failure}: {}",
                    log_tail(&log_path)
                )));
            }
            if spec.socket.exists()
                && interface_ready(&log_path, spec.host_config.shared_ipv6_prefix).is_some()
            {
                // Recheck after reading the completion record: a helper that
                // exited during publication cannot launch the workload.
                match child.try_wait() {
                    Ok(None) => break,
                    Ok(Some(status)) => {
                        self.stop_helper(spec);
                        let _ = child.wait();
                        return Err(Error::unavailable(format!(
                            "clankerd-network exited ({status}) before attachment: {}",
                            log_tail(&log_path)
                        )));
                    }
                    Err(error) => {
                        self.stop_helper(spec);
                        let _ = child.wait();
                        return Err(Error::unavailable(format!(
                            "waiting for clankerd-network: {error}"
                        )));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let ready = interface_ready(&log_path, spec.host_config.shared_ipv6_prefix)
            .ok_or_else(|| Error::unavailable("native attachment readiness record disappeared"))?;
        let pid = ready.pid;
        if let Err(error) = fs::write(spec.dir.join(PID_FILE), pid.to_string()) {
            self.stop_helper(spec);
            return Err(error.into());
        }
        let prefix = spec.host_config.shared_ipv6_prefix;
        if let Err(error) = super::pf::register(spec, pid, prefix)
            .and_then(|_| super::recovery::record(binary, spec))
        {
            self.stop_helper(spec);
            super::pf::remove(spec);
            let _ = child.wait();
            return Err(error);
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
        super::recovery::remove(spec);
        self.stop_helper(spec);
        super::pf::remove(spec);
    }

    fn connected(&self, spec: &NetSpec) -> bool {
        self.running(spec) && super::recovery::healthy(spec)
    }
}

impl VmnetBackend {
    pub(crate) fn running(&self, spec: &NetSpec) -> bool {
        fs::read_to_string(spec.dir.join(PID_FILE))
            .ok()
            .and_then(|pid| pid.trim().parse::<i32>().ok())
            .is_some_and(|pid| is_helper(pid, &spec.socket))
    }

    fn stop_helper(&self, spec: &NetSpec) {
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

/// Guards against a stale pid file: require the caller's helper and exact socket.
fn is_helper(pid: i32, socket: &Path) -> bool {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    if pid <= 0 {
        return false;
    }
    let pid = Pid::from_u32(pid as u32);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing()
            .without_tasks()
            .with_user(UpdateKind::Always)
            .with_exe(UpdateKind::Always)
            .with_cmd(UpdateKind::Always),
    );
    let Some(process) = system.process(pid) else {
        return false;
    };
    // The local protocol stand-in is interpreted, with its script in argv[1].
    let helper = std::ffi::OsStr::new("clankerd-network");
    // SAFETY: reads the current effective identity only.
    process
        .effective_user_id()
        .is_some_and(|uid| **uid == unsafe { libc::geteuid() })
        && (process.exe().and_then(Path::file_name) == Some(helper)
            || process
                .cmd()
                .get(1)
                .is_some_and(|arg| Path::new(arg).file_name() == Some(helper)))
        && process
            .cmd()
            .windows(2)
            .any(|pair| pair[0] == "--socket" && pair[1] == socket.as_os_str())
}

fn log_tail(path: &Path) -> String {
    let log = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(5)..];
    if tail.is_empty() {
        "it printed nothing (see clankerd-network.log in the machine directory)".into()
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

/// Completion is one flushed record identifying the unprivileged transport,
/// its usable interface, and the exact resource-selected prefix.
#[derive(serde::Deserialize)]
struct InterfaceReady {
    pid: u32,
    vmnet_mac_address: String,
    vmnet_mtu: u64,
    vmnet_max_packet_size: u64,
    vmnet_nat66_prefix: Option<std::net::Ipv6Addr>,
}
fn interface_ready(
    path: &Path,
    expected_prefix: Option<std::net::Ipv6Addr>,
) -> Option<InterfaceReady> {
    let bytes = fs::read(path).ok()?;
    String::from_utf8_lossy(&bytes)
        .split_inclusive('\n')
        .find_map(|line| {
            if !line.ends_with('\n') {
                return None;
            }
            let info: InterfaceReady = serde_json::from_str(line.trim()).ok()?;
            let mac: Vec<u8> = info
                .vmnet_mac_address
                .split(':')
                .map(|part| {
                    if part.len() == 2 {
                        u8::from_str_radix(part, 16).ok()
                    } else {
                        None
                    }
                })
                .collect::<Option<_>>()?;
            (info.pid > 1
                && info.vmnet_nat66_prefix == expected_prefix
                && mac.len() == 6
                && mac[0] & 1 == 0
                && mac.iter().any(|byte| *byte != 0)
                && (576..=65535).contains(&info.vmnet_mtu)
                && info.vmnet_max_packet_size >= info.vmnet_mtu + 14)
                .then_some(info)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_identity_preserves_spaces_and_requires_the_exact_socket() {
        use std::io::BufRead;
        let directory = tempfile::Builder::new()
            .prefix("helper with spaces ")
            .tempdir()
            .unwrap();
        let helper = directory.path().join("clankerd-network");
        let socket = directory.path().join("socket with spaces.net");
        fs::write(
            &helper,
            "import time\nprint('ready', flush=True)\ntime.sleep(30)\n",
        )
        .unwrap();
        let mut child = Command::new("python3")
            .arg(&helper)
            .arg("--socket")
            .arg(&socket)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut ready = String::new();
        std::io::BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        let pid = child.id() as i32;
        let matches = is_helper(pid, &socket);
        let wrong_socket = is_helper(pid, &socket.with_extension("other"));
        let unrelated = is_helper(std::process::id() as i32, &socket);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(ready.trim(), "ready");
        assert!(matches);
        assert!(!wrong_socket);
        assert!(!unrelated);
        assert!(!is_helper(pid, &socket));
        assert!(!is_helper(0, &socket));
        assert!(!is_helper(-1, &socket));
    }

    #[test]
    fn readiness_requires_a_complete_identity_interface_and_selected_prefix() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("readiness.log");
        let valid = serde_json::json!({"pid":123, "vmnet_mac_address":"02:00:00:00:00:01",
            "vmnet_mtu":1500, "vmnet_max_packet_size":1514, "vmnet_nat66_prefix":"fd00:1::"});
        let prefix = Some("fd00:1::".parse().unwrap());
        fs::write(&path, format!("diagnostic text\n{valid}\n")).unwrap();
        assert_eq!(interface_ready(&path, prefix).unwrap().pid, 123);
        for (field, value) in [
            ("pid", serde_json::json!(0)),
            ("pid", serde_json::Value::Null),
            ("vmnet_mac_address", serde_json::json!("00:00:00:00:00:00")),
            ("vmnet_mac_address", serde_json::json!("01:00:00:00:00:01")),
            ("vmnet_mtu", serde_json::json!(575)),
            ("vmnet_max_packet_size", serde_json::json!(1500)),
            ("vmnet_nat66_prefix", serde_json::json!("fd00:2::")),
            ("vmnet_nat66_prefix", serde_json::Value::Null),
        ] {
            let mut record = valid.clone();
            record[field] = value;
            fs::write(&path, format!("{record}\n")).unwrap();
            assert!(interface_ready(&path, prefix).is_none(), "{record}");
        }
        fs::write(&path, valid.to_string()).unwrap();
        assert!(interface_ready(&path, prefix).is_none(), "unflushed record");
        let mut host_only = valid;
        host_only["vmnet_nat66_prefix"] = serde_json::Value::Null;
        fs::write(&path, format!("{host_only}\n")).unwrap();
        assert!(interface_ready(&path, None).is_some());
    }
}
