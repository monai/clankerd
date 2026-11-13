use clankerd_network_ipc::{Connection, Request};
use std::{
    fs, io,
    os::{fd::AsRawFd, unix::net::UnixDatagram},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub struct Relay {
    socket: PathBuf,
    active: Arc<AtomicBool>,
}
impl Relay {
    pub fn start(dir: &Path, upstream: &Path) -> io::Result<Option<Self>> {
        let lease = match fs::read(dir.join(super::vmnet::LEASE)) {
            Ok(bytes) => serde_json::from_slice::<super::vmnet::Lease>(&bytes)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        if lease.key != upstream.to_string_lossy() {
            return Err(io::Error::other("network lease does not match VM"));
        }
        let connection = Connection::connect()?;
        let (_, descriptor) = connection.request(&Request::Attach {
            key: lease.key,
            generation: lease.generation,
        })?;
        let back = UnixDatagram::from(
            descriptor.ok_or_else(|| io::Error::other("daemon returned no packet socket"))?,
        );
        let path = upstream.with_extension("relay");
        let _ = fs::remove_file(&path);
        let front = UnixDatagram::bind(&path)?;
        front.set_nonblocking(true)?;
        back.set_nonblocking(true)?;
        let active = Arc::new(AtomicBool::new(true));
        let running = active.clone();
        std::thread::spawn(move || {
            let _connection = connection;
            forward(front, back, running, lease.host);
        });
        Ok(Some(Self {
            socket: path,
            active,
        }))
    }
    pub fn socket(&self) -> &Path {
        &self.socket
    }
}
impl Drop for Relay {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        let _ = fs::remove_file(&self.socket);
    }
}
fn forward(
    front: UnixDatagram,
    back: UnixDatagram,
    active: Arc<AtomicBool>,
    host: crate::HostConfig,
) {
    let mut connected = false;
    let mut buffer = [0u8; 65536];
    while active.load(Ordering::Relaxed) {
        let mut descriptors = [
            libc::pollfd {
                fd: front.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: back.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        if unsafe { libc::poll(descriptors.as_mut_ptr(), 2, 100) } < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            eprintln!(
                "clankerd-vmspawn: VM relay poll: {}",
                io::Error::last_os_error()
            );
            break;
        }
        if descriptors
            .iter()
            .any(|p| p.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0)
        {
            eprintln!(
                "clankerd-vmspawn: VM relay poll flags front={:#x} daemon={:#x}",
                descriptors[0].revents, descriptors[1].revents
            );
            break;
        }
        if descriptors[0].revents & libc::POLLIN != 0 {
            let result = if connected {
                front.recv(&mut buffer)
            } else {
                receive_from(&front, &mut buffer).and_then(|(size, address)| {
                    address.connect(&front)?;
                    connected = true;
                    Ok(size)
                })
            };
            match result {
                Ok(size) if size >= 14 && super::egress::permits(&buffer[..size], &host) => {
                    if let Err(e) = back.send(&buffer[..size])
                        && !clankerd_network_ipc::is_packet_congestion(&e)
                    {
                        eprintln!("clankerd-vmspawn: VM relay send size={size}: {e}");
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    eprintln!("clankerd-vmspawn: VM relay receive/connect: {error}");
                    break;
                }
            }
        }
        if descriptors[1].revents & libc::POLLIN != 0 {
            match back.recv(&mut buffer) {
                Ok(size) if connected => {
                    if let Err(e) = front.send(&buffer[..size])
                        && !clankerd_network_ipc::is_packet_congestion(&e)
                    {
                        eprintln!("clankerd-vmspawn: VM relay send size={size}: {e}");
                        break;
                    }
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => {
                    eprintln!("clankerd-vmspawn: VM relay receive/connect: {error}");
                    break;
                }
            }
        }
    }
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
    use std::time::Duration;

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

    #[test]
    fn packets_flow_both_ways_and_backend_loss_ends_relay_without_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let front_path = dir.path().join("front");
        let vm_path = dir.path().join("vm");
        let front = UnixDatagram::bind(&front_path).unwrap();
        front.set_nonblocking(true).unwrap();
        let vm = UnixDatagram::unbound().unwrap();
        let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
        address.sun_family = libc::AF_UNIX as _;
        let path = vm_path.as_os_str().as_bytes();
        for (destination, &byte) in address.sun_path.iter_mut().zip(path) {
            *destination = byte as _;
        }
        let length =
            (std::mem::offset_of!(libc::sockaddr_un, sun_path) + path.len()) as libc::socklen_t;
        #[cfg(target_os = "macos")]
        {
            address.sun_len = length as u8;
        }
        assert_eq!(
            unsafe {
                libc::bind(
                    vm.as_raw_fd(),
                    (&address as *const libc::sockaddr_un).cast(),
                    length,
                )
            },
            0
        );
        vm.connect(front_path).unwrap();
        vm.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let (back, daemon) = UnixDatagram::pair().unwrap();
        back.set_nonblocking(true).unwrap();
        daemon
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let running = active.clone();
        let worker =
            std::thread::spawn(move || forward(front, back, running, crate::HostConfig::default()));
        let mut frame = [0u8; 42];
        frame[12..14].copy_from_slice(&[8, 6]);
        vm.send(&frame).unwrap();
        let mut received = [0u8; 64];
        assert_eq!(daemon.recv(&mut received).unwrap(), frame.len());
        assert_eq!(&received[..frame.len()], &frame);
        frame[0] = 2;
        daemon.send(&frame).unwrap();
        assert_eq!(vm.recv(&mut received).unwrap(), frame.len());
        assert_eq!(&received[..frame.len()], &frame);
        drop(daemon);
        vm.send(&frame).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !worker.is_finished() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let ended = worker.is_finished();
        active.store(false, Ordering::Relaxed);
        worker.join().unwrap();
        assert!(ended, "relay must stop when the packet backend disappears");
    }
}
