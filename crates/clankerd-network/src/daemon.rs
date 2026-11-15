use crate::interface::{self, Interface};
use clankerd_network_ipc::{Peer, Reply, Request, Response, Server};
use std::{
    collections::BTreeMap,
    io::Read,
    net::Ipv6Addr,
    os::unix::net::UnixDatagram,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

struct Entry {
    generation: String,
    prefix: Option<Ipv6Addr>,
    active: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    wake: Arc<UnixDatagram>,
    peer: Option<UnixDatagram>,
    connection: Option<u64>,
    firewall: bool,
    created: Instant,
    cleanup_attempted: bool,
}
impl Drop for Entry {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        let _ = self.wake.send(&[0]);
        if let Some(worker) = self.worker.take() {
            worker.join().unwrap();
        }
    }
}
struct AttachmentSlot {
    entry: Mutex<Option<Entry>>,
    status: Mutex<Option<(String, Arc<AtomicBool>)>>,
}
type Slot = Arc<AttachmentSlot>;
type Registry = BTreeMap<(u32, String), Slot>;
static INTERFACES: OnceLock<Mutex<Registry>> = OnceLock::new();
fn registry() -> &'static Mutex<Registry> {
    INTERFACES.get_or_init(Mutex::default)
}
fn remove(slot: &Slot, key: &(u32, String)) -> Result<(), String> {
    let mut guard = slot.entry.lock().unwrap();
    if let Some(entry) = guard.as_mut() {
        entry.cleanup_attempted = true;
        entry.active.store(false, Ordering::Relaxed);
        let _ = entry.wake.send(&[0]);
        if let Some(worker) = entry.worker.take() {
            worker
                .join()
                .map_err(|_| "network worker failed".to_string())?;
        }
        if entry.firewall {
            crate::pf::apply(key.0, crate::pf::Change::Remove { key: key.1.clone() })
                .map_err(|error| format!("network firewall cleanup for {}: {error}", key.1))?;
        }
        *guard = None;
    }
    let mut entries = registry().lock().unwrap();
    if entries
        .get(key)
        .is_some_and(|current| Arc::ptr_eq(current, slot))
    {
        entries.remove(key);
    }
    Ok(())
}
fn slots() -> Vec<((u32, String), Slot)> {
    registry()
        .lock()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn request(request: Request, peer: Peer) -> Result<Reply, String> {
    let Peer {
        uid, connection, ..
    } = peer;
    let mut response = Response::default();
    match request {
        Request::List {} => response.interfaces = interface::list()?,
        Request::Create { key, settings } => {
            let key = (uid, key);
            let slot = Arc::new(AttachmentSlot {
                entry: Mutex::new(None),
                status: Mutex::new(None),
            });
            let mut pending = slot.entry.lock().unwrap();
            {
                let mut entries = registry().lock().unwrap();
                if entries.contains_key(&key) {
                    return Err("network already exists; stop the VM before restarting it".into());
                }
                entries.insert(key.clone(), slot.clone());
            }
            let result = (|| -> Result<Entry, String> {
                let policy = crate::egress::Policy::new(&settings)?;
                let (interface, prefix) = Interface::create(&settings)?;
                let wake = interface.wake();
                let (socket, peer) = crate::packets::socket_pair().map_err(|e| e.to_string())?;
                let mut random = [0u8; 16];
                std::fs::File::open("/dev/urandom")
                    .and_then(|mut f| f.read_exact(&mut random))
                    .map_err(|e| e.to_string())?;
                let generation: String = random.iter().map(|b| format!("{b:02x}")).collect();
                if settings.firewall {
                    crate::pf::apply(
                        uid,
                        crate::pf::Change::Register {
                            key: key.1.clone(),
                            network_id: settings.network_id.ok_or("missing network identity")?,
                            subnet: settings.subnet,
                            prefix,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                }
                let active = Arc::new(AtomicBool::new(true));
                let running = active.clone();
                let worker = std::thread::spawn(move || {
                    match crate::packets::forward(interface, socket, running.clone(), policy) {
                        Ok(counters) => eprintln!("clankerd-network: packets {counters:?}"),
                        Err(error) => eprintln!("clankerd-network: packet forwarding: {error}"),
                    }
                    running.store(false, Ordering::Relaxed);
                });
                response.generation = Some(generation.clone());
                response.prefix = prefix.map(|prefix| prefix.to_string());
                response.connected = true;
                Ok(Entry {
                    generation,
                    prefix,
                    active,
                    worker: Some(worker),
                    wake,
                    peer: Some(peer),
                    connection: None,
                    firewall: settings.firewall,
                    created: Instant::now(),
                    cleanup_attempted: false,
                })
            })();
            match result {
                Ok(entry) => {
                    *slot.status.lock().unwrap() =
                        Some((entry.generation.clone(), entry.active.clone()));
                    *pending = Some(entry);
                }
                Err(error) => {
                    registry().lock().unwrap().remove(&key);
                    return Err(error);
                }
            }
        }
        Request::Attach { key, generation } => {
            let slot = registry()
                .lock()
                .unwrap()
                .get(&(uid, key))
                .cloned()
                .ok_or("network unavailable; restart the VM manually")?;
            let mut guard = slot.entry.lock().unwrap();
            let entry = guard
                .as_mut()
                .filter(|e| e.generation == generation && e.active.load(Ordering::Relaxed))
                .ok_or("network unavailable; restart the VM manually")?;
            let peer = entry.peer.take().ok_or("network is already attached")?;
            entry.connection = Some(connection);
            response.connected = true;
            response.prefix = entry.prefix.map(|prefix| prefix.to_string());
            return Ok(Reply {
                response,
                packet: Some(peer.into()),
            });
        }
        Request::Stop { key, generation } => {
            let key = (uid, key);
            let slot = registry().lock().unwrap().get(&key).cloned();
            if let Some(slot) = slot {
                let matches = slot
                    .entry
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|e| e.generation == generation);
                if matches {
                    remove(&slot, &key)?;
                }
            }
        }
        Request::Status { key, generation } => {
            let slot = registry().lock().unwrap().get(&(uid, key)).cloned();
            if let Some(slot) = slot {
                response.connected =
                    slot.status
                        .lock()
                        .unwrap()
                        .as_ref()
                        .is_some_and(|(current, active)| {
                            current == &generation && active.load(Ordering::Relaxed)
                        });
            }
        }
    }
    Ok(Reply {
        response,
        packet: None,
    })
}
fn disconnected(connection: u64) {
    for (key, slot) in slots() {
        let owned = slot
            .entry
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|e| e.connection == Some(connection));
        if owned {
            if let Err(error) = remove(&slot, &key) {
                eprintln!(
                    "{error}; cleanup remains tracked; automatic attempt exhausted, retry an explicit Stop"
                );
            }
        }
    }
}
fn shutdown() {
    for (key, slot) in slots() {
        if let Err(error) = remove(&slot, &key) {
            eprintln!(
                "{error}; cleanup remains tracked; automatic attempt exhausted, retry an explicit Stop"
            );
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
            for (key, slot) in slots() {
                let stopped = slot.entry.try_lock().ok().is_some_and(|mut guard| {
                    guard.as_mut().is_some_and(|e| {
                        let expired = !e.cleanup_attempted
                            && (!e.active.load(Ordering::Relaxed)
                                || (e.connection.is_none()
                                    && e.created.elapsed() >= Duration::from_secs(120)));
                        if expired {
                            e.active.store(false, Ordering::Relaxed);
                        }
                        expired
                    })
                });
                if stopped {
                    if let Err(error) = remove(&slot, &key) {
                        eprintln!(
                            "{error}; cleanup remains tracked; automatic attempt exhausted, retry an explicit Stop"
                        );
                    }
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
