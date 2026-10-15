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
    let diagnostics = std::env::var_os("CLANKERD_NET_DIAGNOSTICS").is_some();
    let mut counts = [0u64; 4]; // front receive, backend send, backend receive, front send.
    let mut errors = [0u64; 4];
    let mut sampled = Instant::now();
    let mut poll_errors = 0;
    let mut peer_errors = 0;
    if diagnostics {
        eprintln!(
            "native relay: front={:?} backend={:?} upstream={} helper_pid={helper_pid}",
            front.local_addr(),
            back.as_ref().map(UnixDatagram::local_addr),
            config.spec.socket.display()
        );
    }
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
        let polled = unsafe { libc::poll(descriptors.as_mut_ptr(), 2, 100) };
        if polled < 0 {
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                poll_errors += 1;
                if poll_errors <= 3 {
                    eprintln!(
                        "native relay: poll failed: {error}; fds={:?}",
                        [descriptors[0].fd, descriptors[1].fd]
                    );
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
        if diagnostics && sampled.elapsed() >= Duration::from_secs(3) {
            eprintln!(
                "native relay: counts front_rx={} back_tx={} back_rx={} front_tx={} errors={errors:?} peer={peer} poll={polled} flags={:?}",
                counts[0],
                counts[1],
                counts[2],
                counts[3],
                [descriptors[0].revents, descriptors[1].revents]
            );
            sampled = Instant::now();
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            match receive_from(&front, &mut buffer) {
                Ok((count, address)) => {
                    counts[0] += 1;
                    if diagnostics && counts[0] <= 3 {
                        eprintln!(
                            "native relay: guest frame len={count} sender={address:?} ethernet={:02x?}",
                            &buffer[..count.min(14)]
                        );
                    }
                    if !peer {
                        match address.connect(&front) {
                            Ok(()) => {
                                peer = true;
                                if diagnostics {
                                    eprintln!("native relay: connected guest peer {address:?}");
                                }
                            }
                            Err(error) => {
                                peer_errors += 1;
                                if peer_errors <= 3 {
                                    eprintln!(
                                        "native relay: cannot connect guest peer {address:?}: {error}"
                                    );
                                }
                            }
                        }
                    }
                    if !super::egress::permits(&buffer[..count], &config.spec.host_config) {
                        if diagnostics {
                            eprintln!(
                                "native relay: rejected source outside selected guest network"
                            );
                        }
                        continue;
                    }
                    if let Some(socket) = &back {
                        match socket.send(&buffer[..count]) {
                            Ok(_) => counts[1] += 1,
                            Err(error) => {
                                errors[1] += 1;
                                if diagnostics && errors[1] <= 3 {
                                    eprintln!(
                                        "native relay: helper send failed len={count}: {error}"
                                    );
                                }
                                if error.kind() != io::ErrorKind::WouldBlock {
                                    eprintln!(
                                        "clankerd-vmspawn: native Ethernet attachment lost: {error}"
                                    );
                                    back = None;
                                    let _ = fs::write(config.spec.dir.join(HEALTH), b"0");
                                    retry_at = Instant::now() + delay;
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    errors[0] += 1;
                    if diagnostics && errors[0] <= 3 {
                        eprintln!("native relay: guest receive failed: {error}");
                    }
                }
            }
        }
        if descriptors[1].revents & libc::POLLIN != 0
            && let Some(socket) = &back
        {
            match socket.recv(&mut buffer) {
                Ok(count) => {
                    counts[2] += 1;
                    if diagnostics && counts[2] <= 3 {
                        eprintln!(
                            "native relay: helper frame len={count} ethernet={:02x?}",
                            &buffer[..count.min(14)]
                        );
                    }
                    if peer {
                        match front.send(&buffer[..count]) {
                            Ok(_) => counts[3] += 1,
                            Err(error) => {
                                errors[3] += 1;
                                if diagnostics && errors[3] <= 3 {
                                    eprintln!(
                                        "native relay: guest send failed len={count}: {error}"
                                    );
                                }
                            }
                        }
                    }
                }
                Err(error) => {
                    errors[2] += 1;
                    if diagnostics && errors[2] <= 3 {
                        eprintln!("native relay: helper receive failed: {error}");
                    }
                }
            }
        }
    }
    if diagnostics {
        eprintln!(
            "native relay: stopped active={} config_exists={} counts={counts:?} errors={errors:?}",
            active.load(Ordering::Relaxed),
            config.spec.dir.join(CONFIG).exists()
        );
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

// Preserve the kernel address and its exact length. Darwin returns a libkrun
// peer pathname without a NUL terminator; converting through SocketAddr's
// as_pathname drops its final byte, so that path cannot be used to reconnect.
struct RawPeer {
    address: libc::sockaddr_storage,
    length: libc::socklen_t,
}
impl RawPeer {
    fn connect(&self, socket: &UnixDatagram) -> io::Result<()> {
        if self.address.ss_family as i32 != libc::AF_UNIX
            || self.length as usize <= std::mem::offset_of!(libc::sockaddr_un, sun_path)
            || self.length as usize > std::mem::size_of_val(&self.address)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid Unix datagram peer address",
            ));
        }
        // SAFETY: initialized address storage covers exactly length bytes;
        // socket owns its descriptor. No pathname decoding changes the address.
        let result = unsafe {
            libc::connect(
                socket.as_raw_fd(),
                (&self.address as *const libc::sockaddr_storage).cast(),
                self.length,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}
impl std::fmt::Debug for RawPeer {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let offset = std::mem::offset_of!(libc::sockaddr_un, sun_path);
        let length = (self.length as usize).min(std::mem::size_of_val(&self.address));
        // SAFETY: RawPeer is entirely initialized by zeroing before recvfrom.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&self.address as *const libc::sockaddr_storage).cast::<u8>(),
                length,
            )
        };
        let path = bytes.get(offset..).unwrap_or_default();
        let path = path.strip_suffix(&[0]).unwrap_or(path);
        formatter
            .debug_struct("RawPeer")
            .field("path", &String::from_utf8_lossy(path))
            .field("length", &self.length)
            .finish()
    }
}
fn receive_from(socket: &UnixDatagram, buffer: &mut [u8]) -> io::Result<(usize, RawPeer)> {
    // SAFETY: all zero bytes are a valid initialized sockaddr_storage.
    let mut address: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut length = std::mem::size_of_val(&address) as libc::socklen_t;
    // SAFETY: the buffer and address storage are writable for their passed sizes.
    let count = unsafe {
        libc::recvfrom(
            socket.as_raw_fd(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
            (&mut address as *mut libc::sockaddr_storage).cast(),
            &mut length,
        )
    };
    if count < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok((count as usize, RawPeer { address, length }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStrExt;

    #[test]
    fn raw_non_nul_peer_address_routes_a_reply_to_the_complete_socket_name() {
        let directory = tempfile::tempdir().unwrap();
        let peer_path = directory.path().join("krun-net-123-0.sock");
        let peer = UnixDatagram::unbound().unwrap();
        // Bind with libkrun's exact pathname length, excluding a terminator.
        // SAFETY: all-zero storage is a valid initialized sockaddr_un.
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as _;
        let path = peer_path.as_os_str().as_bytes();
        assert!(path.len() < address.sun_path.len());
        for (destination, &byte) in address.sun_path.iter_mut().zip(path) {
            *destination = byte as _;
        }
        let length =
            (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len()) as libc::socklen_t;
        #[cfg(target_os = "macos")]
        {
            address.sun_len = length as u8;
        }
        // SAFETY: address storage includes all bytes covered by length; peer owns its descriptor.
        assert_eq!(
            unsafe {
                libc::bind(
                    peer.as_raw_fd(),
                    (&address as *const libc::sockaddr_un).cast(),
                    length,
                )
            },
            0
        );
        let front_path = directory.path().join("front.net");
        let front = UnixDatagram::bind(&front_path).unwrap();
        peer.connect(&front_path).unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        peer.send(b"request").unwrap();
        let mut buffer = [0u8; 64];
        let (_, mut sender) = receive_from(&front, &mut buffer).unwrap();
        // Linux adds a terminator to the reported address; Darwin preserves
        // the bind length. Exercise the latter contract on either host.
        sender.length = length;
        sender.connect(&front).unwrap();
        front.send(b"reply").unwrap();
        let count = peer.recv(&mut buffer).unwrap();
        assert_eq!(&buffer[..count], b"reply");
    }
}
