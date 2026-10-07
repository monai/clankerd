//! The gvproxy (gvisor-tap-vsock) backend: one `gvproxy` process per machine
//! in vfkit mode, a unix datagram socket the VMM's virtio-net device connects
//! to (`krun_add_net_unixgram` with `NET_FLAG_VFKIT`).
//!
//! gvproxy runs with its default configuration, which gives the guest
//! 192.168.127.2/24 by DHCP for MAC [`GUEST_MAC`], gateway and DNS 192.168.127.1
//! and a DNS service that forwards to the host's resolvers. Two things are
//! switched off on purpose:
//!
//! * No `-listen`/`-services`: gvproxy's host-side HTTP control API is never
//!   started, so nothing on the host can be driven through it.
//! * `-ssh-port -1`: the default forward of host 127.0.0.1:2222 to guest SSH.
//!
//! gvproxy also always serves a *second* copy of its forwarder API inside the
//! virtual network at `192.168.127.1:80` (`/services/forwarder/expose`), which
//! would let anything in the guest open listeners on the Mac, and aliases
//! 192.168.127.254 to the host's 127.0.0.1. Neither can be disabled by flag,
//! so [`GvproxyBackend::start`] reports both as [`blocked`](NetAttachment::blocked)
//! destinations and guestd drops them in nftables before the workload starts.

use std::fs::{self, OpenOptions};
use std::net::Ipv4Addr;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use clankerd_proto::guest::BlockedEndpoint;

use super::{NetAttachment, NetBackend, NetSpec};
use crate::error::{Error, Result};
use crate::net::GvproxyFetcher;

/// gvproxy's built-in static DHCP lease (192.168.127.2) is keyed to this MAC.
pub const GUEST_MAC: [u8; 6] = [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee];
/// libkrun's `COMPAT_NET_FEATURES`: CSUM, GUEST_CSUM, GUEST_TSO4, GUEST_UFO,
/// HOST_TSO4, HOST_UFO. Set to 0 to run without offloads if throughput or
/// stability disappoints on the M2.
pub const NET_FEATURES: u32 = (1 << 0) | (1 << 1) | (1 << 7) | (1 << 10) | (1 << 11) | (1 << 14);

const GATEWAY: Ipv4Addr = Ipv4Addr::new(192, 168, 127, 1);
/// gvproxy's default NAT entry: the last usable address of the subnet is the host's loopback.
const HOST_LOOPBACK_ALIAS: Ipv4Addr = Ipv4Addr::new(192, 168, 127, 254);

const PID_FILE: &str = "net.pid";
const LOG_FILE: &str = "gvproxy.log";
const READY_TIMEOUT: Duration = Duration::from_secs(10);
const STOP_GRACE: Duration = Duration::from_secs(3);

enum Source {
    Binary(PathBuf),
    Fetch(GvproxyFetcher),
}

pub struct GvproxyBackend {
    source: Source,
}

impl GvproxyBackend {
    /// Uses the gvproxy binary at `binary`.
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        GvproxyBackend {
            source: Source::Binary(binary.into()),
        }
    }

    /// Downloads (and caches) the pinned release when the first machine starts.
    pub fn fetching(fetcher: GvproxyFetcher) -> Self {
        GvproxyBackend {
            source: Source::Fetch(fetcher),
        }
    }

    fn binary(&self) -> Result<PathBuf> {
        match &self.source {
            Source::Binary(path) => Ok(path.clone()),
            Source::Fetch(fetcher) => fetcher.ensure(),
        }
    }
}

impl NetBackend for GvproxyBackend {
    fn start(&self, spec: &NetSpec) -> Result<NetAttachment> {
        let binary = self.binary()?;
        // A leftover sidecar of an earlier run would hold the socket.
        self.stop(spec);
        let log_path = spec.dir.join(LOG_FILE);
        let log = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&log_path)?;
        let mut child = Command::new(&binary)
            .arg("-listen-vfkit")
            .arg(format!("unixgram://{}", spec.socket.display()))
            .args(["-ssh-port", "-1"])
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .process_group(0)
            .spawn()
            .map_err(|e| {
                Error::unavailable(format!("cannot start gvproxy ({}): {e}", binary.display()))
            })?;
        fs::write(spec.dir.join(PID_FILE), child.id().to_string())?;

        let deadline = Instant::now() + READY_TIMEOUT;
        loop {
            if spec.socket.exists() {
                break;
            }
            let failure = match child.try_wait() {
                Ok(Some(status)) => Some(format!("gvproxy exited ({status}) before it was ready")),
                Ok(None) if Instant::now() >= deadline => {
                    Some(format!("gvproxy not ready after {READY_TIMEOUT:?}"))
                }
                Ok(None) => None,
                Err(e) => Some(format!("waiting for gvproxy: {e}")),
            };
            if let Some(failure) = failure {
                self.stop(spec);
                let _ = child.wait();
                return Err(Error::unavailable(format!(
                    "{failure}: {}",
                    log_tail(&log_path)
                )));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        // Reap it when it ends while we are still alive.
        std::thread::spawn(move || {
            let _ = child.wait();
        });

        Ok(NetAttachment {
            socket: spec.socket.clone(),
            mac: GUEST_MAC,
            features: NET_FEATURES,
            vfkit: true,
            blocked: vec![
                BlockedEndpoint {
                    addr: GATEWAY,
                    tcp_port: Some(80),
                },
                BlockedEndpoint {
                    addr: HOST_LOOPBACK_ALIAS,
                    tcp_port: None,
                },
            ],
        })
    }

    fn stop(&self, spec: &NetSpec) {
        let pid_file = spec.dir.join(PID_FILE);
        if let Some(pid) = fs::read_to_string(&pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<i32>().ok())
            && is_gvproxy(pid)
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
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Guards against a stale pid file (after a host reboot the pid may belong to
/// anything): only signal a process whose command line mentions gvproxy. If
/// `ps` cannot be run the pid file is trusted.
fn is_gvproxy(pid: i32) -> bool {
    if !process_exists(pid) {
        return false;
    }
    match Command::new("ps")
        .args(["-o", "command=", "-p", &pid.to_string()])
        .output()
    {
        Ok(out) if out.status.success() || !out.stdout.is_empty() => {
            String::from_utf8_lossy(&out.stdout).contains("gvproxy")
        }
        Ok(_) => false,
        Err(_) => true,
    }
}

fn log_tail(path: &Path) -> String {
    let log = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = &lines[lines.len().saturating_sub(5)..];
    if tail.is_empty() {
        "it printed nothing (see gvproxy.log in the machine directory)".into()
    } else {
        tail.join(" | ")
    }
}
