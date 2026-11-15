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
    time::Duration,
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
            crate::pf::apply(key.0, crate::pf::Change::Remove { key: key.1.clone() })
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
            response.interfaces = interface::list()?;
        }
        Request::Create { key, settings } => {
            let key = (uid, key);
            if entries.contains_key(&key) {
                return Err("network already exists; stop the VM before restarting it".into());
            }
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
            entries.insert(
                key,
                Entry {
                    generation,
                    prefix,
                    active,
                    worker: Some(worker),
                    wake,
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
            response.prefix = entry.prefix.map(|prefix| prefix.to_string());
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
