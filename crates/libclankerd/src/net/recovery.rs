//! VMM-owned stable Ethernet endpoint and exact-config helper recovery.
use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::{NetBackend, NetSpec, VmnetBackend};
use serde::{Deserialize, Serialize};

const CONFIG: &str = "net-recovery.json";
const HEALTH: &str = "net-connected";
#[derive(Serialize, Deserialize)]
struct Configuration {
    binary: PathBuf,
    spec: NetSpec,
}

pub(super) fn record(binary: &Path, spec: &NetSpec) -> crate::Result<()> {
    let bytes = serde_json::to_vec(&Configuration {
        binary: binary.into(),
        spec: spec.clone(),
    })?;
    let temporary = spec.dir.join("net-recovery.tmp");
    fs::write(&temporary, bytes)?;
    fs::rename(temporary, spec.dir.join(CONFIG))?;
    Ok(())
}
pub(super) fn healthy(spec: &NetSpec) -> bool {
    fs::read_to_string(spec.dir.join(HEALTH)).is_ok_and(|value| value == "1")
        || !spec.socket.with_extension("relay").exists()
}
pub(super) fn remove(spec: &NetSpec) {
    for path in [
        spec.dir.join(CONFIG),
        spec.dir.join(HEALTH),
        spec.socket.with_extension("relay"),
        spec.socket.with_extension("relay-peer"),
    ] {
        let _ = fs::remove_file(path);
    }
}

/// A stable endpoint owned by the native VMM, so recovery outlives every CLI.
pub struct Relay {
    socket: PathBuf,
    running: Arc<AtomicBool>,
}
impl Relay {
    /// Fixture sockets remain untouched; only the recorded native backend is supervised.
    pub fn start(dir: &Path, upstream: &Path) -> io::Result<Option<Self>> {
        let config = match fs::read(dir.join(CONFIG)) {
            Ok(bytes) => {
                serde_json::from_slice::<Configuration>(&bytes).map_err(io::Error::other)?
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if config.spec.socket != upstream {
            return Ok(None);
        }
        let path = upstream.with_extension("relay");
        let peer_path = upstream.with_extension("relay-peer");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&peer_path);
        let front = UnixDatagram::bind(&path)?;
        front.set_nonblocking(true)?;
        let back = connect(&peer_path, upstream)?;
        fs::write(dir.join(HEALTH), b"1")?;
        let running = Arc::new(AtomicBool::new(true));
        let active = running.clone();
        std::thread::spawn(move || serve(config, front, back, peer_path, active));
        Ok(Some(Self {
            socket: path,
            running,
        }))
    }
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
    }
}
fn connect(local: &Path, upstream: &Path) -> io::Result<UnixDatagram> {
    let _ = fs::remove_file(local);
    let socket = UnixDatagram::bind(local)?;
    socket.connect(upstream)?;
    socket.set_nonblocking(true)?;
    // vmnet-helper accepts a short frame to establish its Unixgram peer, then discards it.
    socket.send(b"VFKT")?;
    Ok(socket)
}
fn serve(
    config: Configuration,
    front: UnixDatagram,
    back: UnixDatagram,
    peer_path: PathBuf,
    active: Arc<AtomicBool>,
) {
    let backend = VmnetBackend::new(&config.binary);
    let mut back = Some(back);
    let mut helper_pid = pid(&config.spec);
    let mut retry_at = Instant::now();
    let mut delay = Duration::from_secs(2);
    let mut peer = false;
    let mut buffer = [0u8; 65536];
    while active.load(Ordering::Relaxed) && config.spec.dir.join(CONFIG).exists() {
        if back.is_some() && !process_alive(helper_pid) {
            back = None;
            let _ = fs::write(config.spec.dir.join(HEALTH), b"0");
            retry_at = Instant::now() + delay;
        }
        if back.is_none() && Instant::now() >= retry_at {
            match backend
                .start(&config.spec)
                .and_then(|_| connect(&peer_path, &config.spec.socket).map_err(Into::into))
            {
                Ok(socket) => {
                    helper_pid = pid(&config.spec);
                    back = Some(socket);
                    delay = Duration::from_secs(2);
                    let _ = fs::write(config.spec.dir.join(HEALTH), b"1");
                }
                Err(error) => {
                    eprintln!("clankerd-vmspawn: native network recovery: {error}");
                    retry_at = Instant::now() + delay;
                    delay = (delay * 2).min(Duration::from_secs(30));
                }
            }
        }
        let mut descriptors = [
            libc::pollfd {
                fd: front.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: back.as_ref().map_or(-1, AsRawFd::as_raw_fd),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        // SAFETY: descriptors is valid storage for two pollfd entries.
        unsafe {
            libc::poll(descriptors.as_mut_ptr(), 2, 100);
        }
        if descriptors[0].revents & libc::POLLIN != 0
            && let Ok((count, address)) = front.recv_from(&mut buffer)
        {
            if !peer && let Some(path) = address.as_pathname() {
                peer = front.connect(path).is_ok();
            }
            if let Some(socket) = &back
                && let Err(error) = socket.send(&buffer[..count])
                && error.kind() != io::ErrorKind::WouldBlock
            {
                eprintln!("clankerd-vmspawn: native Ethernet attachment lost: {error}");
                back = None;
                let _ = fs::write(config.spec.dir.join(HEALTH), b"0");
                retry_at = Instant::now() + delay;
            }
        }
        if descriptors[1].revents & libc::POLLIN != 0
            && let Some(socket) = &back
            && let Ok(count) = socket.recv(&mut buffer)
            && peer
        {
            let _ = front.send(&buffer[..count]);
        }
    }
    if let Ok(address) = front.local_addr()
        && let Some(path) = address.as_pathname()
    {
        let _ = fs::remove_file(path);
    }
    let _ = fs::remove_file(peer_path);
}
fn pid(spec: &NetSpec) -> i32 {
    fs::read_to_string(spec.dir.join("net.pid"))
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(0)
}
fn process_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal zero only observes the sidecar process recorded by startup.
    (unsafe { libc::kill(pid, 0) == 0 })
        || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
