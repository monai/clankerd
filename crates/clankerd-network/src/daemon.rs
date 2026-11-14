use clankerd_network_ipc::{Peer, Reply, Request, Response, Server, Settings};
use std::{
    collections::BTreeMap,
    ffi::{CStr, CString, c_char, c_void},
    io::{self, Read},
    os::fd::AsRawFd,
    os::unix::net::UnixDatagram,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};

unsafe extern "C" {
    fn clankerd_vmnet_create(
        identity: *const c_char,
        mode: *const c_char,
        start: *const c_char,
        end: *const c_char,
        mask: *const c_char,
        prefix: *const c_char,
        shared: *const c_char,
        actual_prefix: *mut *mut c_char,
    ) -> *mut c_void;
    fn clankerd_vmnet_stop(handle: *mut c_void) -> i32;
    fn clankerd_vmnet_read(handle: *mut c_void, buffer: *mut u8, size: *mut usize) -> i32;
    fn clankerd_vmnet_write(handle: *mut c_void, buffer: *mut u8, size: usize) -> i32;
    fn clankerd_vmnet_list() -> *mut c_char;
}
struct Interface(*mut c_void);
unsafe impl Send for Interface {}
impl Drop for Interface {
    fn drop(&mut self) {
        unsafe { clankerd_vmnet_stop(self.0) };
    }
}
impl Interface {
    fn create(settings: &Settings) -> Result<(Self, Option<String>), String> {
        let strings = [
            Some(settings.interface_id.as_str()),
            Some(settings.operation_mode.as_str()),
            settings.start_address.as_deref(),
            settings.end_address.as_deref(),
            settings.subnet_mask.as_deref(),
            settings.nat66_prefix.as_deref(),
            settings.shared_interface.as_deref(),
        ]
        .map(|s| s.map(CString::new).transpose())
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())?;
        let p: Vec<_> = strings
            .iter()
            .map(|s| s.as_ref().map_or(std::ptr::null(), |s| s.as_ptr()))
            .collect();
        let mut prefix = std::ptr::null_mut();
        let handle =
            unsafe { clankerd_vmnet_create(p[0], p[1], p[2], p[3], p[4], p[5], p[6], &mut prefix) };
        if handle.is_null() {
            return Err("vmnet interface creation failed".into());
        }
        let actual = if prefix.is_null() {
            None
        } else {
            let text = unsafe { CStr::from_ptr(prefix).to_string_lossy().into_owned() };
            unsafe { libc::free(prefix.cast()) };
            Some(text)
        };
        let interface = Self(handle);
        if settings
            .nat66_prefix
            .as_deref()
            .map(str::parse::<std::net::Ipv6Addr>)
            .transpose()
            .map_err(|e| e.to_string())?
            != actual
                .as_deref()
                .map(str::parse::<std::net::Ipv6Addr>)
                .transpose()
                .map_err(|e| e.to_string())?
        {
            return Err("vmnet did not acknowledge the requested IPv6 prefix".into());
        }
        Ok((interface, actual))
    }
    fn relay(self, socket: UnixDatagram, active: Arc<AtomicBool>) {
        let mut buffer = [0u8; 65536];
        while active.load(Ordering::Relaxed) {
            let mut poll = libc::pollfd {
                fd: socket.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let ready = unsafe { libc::poll(&mut poll, 1, 5) };
            if ready < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                eprintln!(
                    "clankerd-network: daemon packet poll: {}",
                    io::Error::last_os_error()
                );
                break;
            }
            if poll.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                eprintln!(
                    "clankerd-network: daemon packet socket poll flags={:#x}",
                    poll.revents
                );
                break;
            }
            if poll.revents & libc::POLLIN != 0 {
                match socket.recv(&mut buffer) {
                    Ok(size) if size >= 14 => {
                        if unsafe { clankerd_vmnet_write(self.0, buffer.as_mut_ptr(), size) } != 0 {
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => {
                        eprintln!("clankerd-network: daemon packet receive: {error}");
                        break;
                    }
                }
            }
            for _ in 0..64 {
                let mut size = 0;
                let result = unsafe { clankerd_vmnet_read(self.0, buffer.as_mut_ptr(), &mut size) };
                if result < 0 {
                    active.store(false, Ordering::Relaxed);
                    break;
                }
                if result == 0 {
                    break;
                }
                if let Err(e) = socket.send(&buffer[..size])
                    && !clankerd_network_ipc::is_packet_congestion(&e)
                {
                    eprintln!("clankerd-network: daemon packet send size={size}: {e}");
                    active.store(false, Ordering::Relaxed);
                    break;
                }
            }
        }
        active.store(false, Ordering::Relaxed);
    }
}
struct Entry {
    generation: String,
    prefix: Option<String>,
    active: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    peer: Option<UnixDatagram>,
    connection: Option<u64>,
    firewall: bool,
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
type Registry = BTreeMap<(u32, String), Entry>;
static INTERFACES: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> &'static Mutex<Registry> {
    INTERFACES.get_or_init(Mutex::default)
}
fn remove(entries: &mut Registry, key: &(u32, String)) -> Result<(), String> {
    if let Some(entry) = entries.remove(key) {
        let firewall = entry.firewall;
        drop(entry);
        if firewall {
            crate::pf::apply(
                key.0,
                crate::pf::Change::Remove {
                    socket: key.1.clone(),
                },
            )
            .map_err(|error| format!("network firewall cleanup for {}: {error}", key.1))?;
        }
    }
    Ok(())
}

fn request(request: Request, peer: Peer) -> Result<Reply, String> {
    let Peer {
        uid, connection, ..
    } = peer;
    let mut entries = registry().lock().unwrap();
    let mut response = Response::default();
    match request {
        Request::List {} => {
            let pointer = unsafe { clankerd_vmnet_list() };
            if pointer.is_null() {
                return Err("cannot list vmnet interfaces".into());
            }
            response.interfaces = unsafe { CStr::from_ptr(pointer) }
                .to_string_lossy()
                .lines()
                .map(str::to_owned)
                .collect();
            unsafe { libc::free(pointer.cast()) };
        }
        Request::Create { key, settings } => {
            let key = (uid, key);
            if entries.contains_key(&key) {
                return Err("network already exists; stop the VM before restarting it".into());
            }
            let (interface, prefix) = Interface::create(&settings)?;
            let (socket, peer) = UnixDatagram::pair().map_err(|e| e.to_string())?;
            socket.set_nonblocking(true).map_err(|e| e.to_string())?;
            let mut random = [0u8; 16];
            std::fs::File::open("/dev/urandom")
                .and_then(|mut f| f.read_exact(&mut random))
                .map_err(|e| e.to_string())?;
            let generation: String = random.iter().map(|b| format!("{b:02x}")).collect();
            if settings.firewall {
                crate::pf::apply(
                    uid,
                    crate::pf::Change::Register {
                        socket: key.1.clone(),
                        network_id: settings.network_id.ok_or("missing network identity")?,
                        subnet: settings.subnet,
                        prefix: prefix
                            .as_deref()
                            .map(str::parse)
                            .transpose()
                            .map_err(|e: std::net::AddrParseError| e.to_string())?,
                    },
                )
                .map_err(|e| e.to_string())?;
            }
            let active = Arc::new(AtomicBool::new(true));
            let running = active.clone();
            let worker = std::thread::spawn(move || interface.relay(socket, running));
            response.generation = Some(generation.clone());
            response.prefix = prefix.clone();
            response.connected = true;
            entries.insert(
                key,
                Entry {
                    generation,
                    prefix,
                    active,
                    worker: Some(worker),
                    peer: Some(peer),
                    connection: None,
                    firewall: settings.firewall,
                },
            );
        }
        Request::Attach { key, generation } => {
            let entry = entries
                .get_mut(&(uid, key))
                .filter(|e| e.generation == generation && e.active.load(Ordering::Relaxed))
                .ok_or("network unavailable; restart the VM")?;
            let peer = entry.peer.take().ok_or("network is already attached")?;
            entry.connection = Some(connection);
            response.connected = true;
            response.prefix = entry.prefix.clone();
            return Ok(Reply {
                response,
                packet: Some(peer.into()),
            });
        }
        Request::Stop { key, generation } => {
            let key = (uid, key);
            if entries
                .get(&key)
                .is_some_and(|e| e.generation == generation)
            {
                remove(&mut entries, &key)?;
            }
        }
        Request::Status { key, generation } => {
            response.connected = entries
                .get(&(uid, key))
                .is_some_and(|e| e.generation == generation && e.active.load(Ordering::Relaxed));
        }
    }
    Ok(Reply {
        response,
        packet: None,
    })
}
fn disconnected(connection: u64) {
    let mut entries = registry().lock().unwrap();
    let keys: Vec<_> = entries
        .iter()
        .filter(|(_, e)| e.connection == Some(connection))
        .map(|(k, _)| k.clone())
        .collect();
    for key in keys {
        if let Err(error) = remove(&mut entries, &key) {
            eprintln!("{error}");
        }
    }
}
fn shutdown() {
    let mut entries = registry().lock().unwrap();
    let keys: Vec<_> = entries.keys().cloned().collect();
    for key in keys {
        if let Err(error) = remove(&mut entries, &key) {
            eprintln!("{error}");
        }
    }
}
pub fn run() -> ! {
    if let Err(error) = crate::pf::reset() {
        eprintln!("cannot reset previous daemon firewall state: {error}");
        std::process::exit(1);
    }
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(5));
            let mut entries = registry().lock().unwrap();
            let stopped: Vec<_> = entries
                .iter()
                .filter(|(_, e)| !e.active.load(Ordering::Relaxed))
                .map(|(k, _)| k.clone())
                .collect();
            for key in stopped {
                if let Err(error) = remove(&mut entries, &key) {
                    eprintln!("{error}");
                }
            }
        }
    });
    clankerd_network_ipc::serve(Server {
        request,
        disconnected,
        shutdown,
    })
}
