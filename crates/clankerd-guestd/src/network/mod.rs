//! Standards-based automatic networking, supervised independently of the workload.
pub mod files;
pub mod hook;
mod iface;
mod peer;
pub mod probe;
mod snapshot;

use clankerd_proto::guest::{NetworkConfig, NetworkStatus};
use std::ffi::CString;
use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
static CLIENT_SPAWN: Mutex<()> = Mutex::new(());
static CLIENT_RUNNING: AtomicBool = AtomicBool::new(false);
static IPV6_PEER: AtomicBool = AtomicBool::new(false);
const INITIAL_WAIT: Duration = Duration::from_secs(20);

pub fn prepare() {
    for error in files::write_sysctls(Path::new("/proc/sys"), &files::FORWARDING_SYSCTLS) {
        eprintln!("clankerd-guestd: {error}");
    }
    if let Err(error) = iface::set_up("lo") {
        eprintln!("clankerd-guestd: bringing lo up: {error}");
    }
}

pub fn setup(config: &NetworkConfig) -> Result<(), String> {
    if config.interface != "eth0" {
        return Err("automatic networking supports the virtio interface eth0".into());
    }
    let hostname = CString::new(config.hostname.as_str()).map_err(|error| error.to_string())?;
    // SAFETY: hostname is a valid buffer with the specified length.
    if unsafe { libc::sethostname(hostname.as_ptr(), config.hostname.len()) } < 0 {
        eprintln!(
            "clankerd-guestd: setting hostname: {}",
            std::io::Error::last_os_error()
        );
    }
    files::write_hostname(Path::new("/"), &config.hostname).map_err(|error| error.to_string())?;
    iface::wait_for(&config.interface, Duration::from_secs(10))
        .map_err(|error| error.to_string())?;
    iface::set_up(&config.interface).map_err(|error| error.to_string())?;
    fs::create_dir_all("/run/clankerd/dhcpcd").map_err(|error| error.to_string())?;
    fs::create_dir_all("/run/clankerd/network").map_err(|error| error.to_string())?;
    hook::refresh().map_err(|error| format!("initializing resolver: {error}"))?;
    // dhcpcd owns RA/SLAAC in userspace, disables kernel accept_ra/autoconf,
    // and remains able to process RA packets while Docker forwarding is enabled.
    // Stable DUID-LL derives from the machine's stable unique MAC, not image state.
    fs::write("/run/clankerd/network/dhcpcd.conf",
        "duid ll\nnoipv4ll\nnosyslog\ntimeout 0\noption domain_name_servers, domain_search\noption dhcp6_name_servers, dhcp6_domain_search\n")
        .map_err(|error|error.to_string())?;
    let mut command = Command::new("/run/clankerd/boot/dhcpcd");
    command
        .args([
            "-B",
            "-f",
            "/run/clankerd/network/dhcpcd.conf",
            "-c",
            "/run/clankerd/boot/clankerd-guestd-network-hook",
            &config.interface,
        ])
        .env_clear()
        .env("PATH", "/run/clankerd/boot")
        .stdin(Stdio::null());
    let mut child = crate::power::spawn_owned(&mut command)
        .map_err(|error| format!("starting bundled dhcpcd: {error}"))?;
    CLIENT_RUNNING.store(true, Ordering::Relaxed);
    let deadline = Instant::now() + INITIAL_WAIT;
    loop {
        match child.try_wait() {
            Ok(Some(exit)) => {
                CLIENT_RUNNING.store(false, Ordering::Relaxed);
                crate::power::release(child.id());
                return Err(format!("dhcpcd exited before networking was ready: {exit}"));
            }
            Err(error) => return stop_failed(&mut child, format!("waiting for dhcpcd: {error}")),
            Ok(None) => {}
        }
        let state = status(&config.interface);
        if state.ready {
            break;
        }
        if !IPV6_PEER.load(Ordering::Relaxed)
            && state.address_details.iter().any(|address| {
                address.address.is_ipv6() && !address.tentative && !address.dad_failed
            })
            && peer::reachable(&config.interface).unwrap_or(false)
        {
            IPV6_PEER.store(true, Ordering::Relaxed);
            continue;
        }
        if Instant::now() >= deadline {
            return stop_failed(
                &mut child,
                format!(
                    "no usable family after {INITIAL_WAIT:?}: {} (addresses {:?})",
                    state.diagnostic, state.address_details
                ),
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    eprintln!(
        "clankerd-guestd: network ready: {}",
        serde_json::to_string(&status(&config.interface)).unwrap()
    );
    probe::start_network(
        config.interface.clone(),
        config.probe_endpoints.clone().unwrap_or_default(),
    );
    // dhcpcd maintains both families; its failure must not stop a running workload.
    let interface = config.interface.clone();
    std::thread::spawn(move || {
        let mut delay = Duration::from_secs(1);
        loop {
            let started = Instant::now();
            let result = child.wait();
            CLIENT_RUNNING.store(false, Ordering::Relaxed);
            IPV6_PEER.store(false, Ordering::Relaxed);
            probe::invalidate();
            crate::power::release(child.id());
            if let Err(error) = hook::clear() {
                eprintln!("clankerd-guestd: clearing client evidence: {error}");
            }
            eprintln!(
                "clankerd-guestd: automatic network configuration client stopped: {result:?}"
            );
            if started.elapsed() >= Duration::from_secs(30) {
                delay = Duration::from_secs(1);
            }
            loop {
                if SHUTTING_DOWN.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(delay);
                let _spawn = CLIENT_SPAWN
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if SHUTTING_DOWN.load(Ordering::Relaxed) {
                    return;
                }
                match crate::power::spawn_owned(&mut command) {
                    Ok(replacement) => {
                        child = replacement;
                        CLIENT_RUNNING.store(true, Ordering::Relaxed);
                        break;
                    }
                    Err(error) => {
                        eprintln!("clankerd-guestd: restarting configuration client: {error}")
                    }
                }
                delay = (delay * 2).min(Duration::from_secs(30));
            }
            delay = (delay * 2).min(Duration::from_secs(30));
        }
    });
    std::thread::spawn(move || {
        loop {
            let reachable = client_running() && peer::reachable(&interface).unwrap_or(false);
            IPV6_PEER.store(reachable, Ordering::Relaxed);
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    std::thread::spawn(|| {
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if let Err(error) = hook::refresh() {
                eprintln!("clankerd-guestd: updating resolver: {error}");
            }
        }
    });
    Ok(())
}

pub fn shutdown() {
    let _spawn = CLIENT_SPAWN
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    SHUTTING_DOWN.store(true, Ordering::Relaxed);
}

fn stop_failed(child: &mut std::process::Child, message: String) -> Result<(), String> {
    // SIGTERM lets dhcpcd withdraw addresses, routes and resolver contributions.
    // SAFETY: child PID is live and owned by this process.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while child.try_wait().ok().flatten().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.kill();
    let _ = child.wait();
    crate::power::release(child.id());
    CLIENT_RUNNING.store(false, Ordering::Relaxed);
    Err(message)
}

pub fn status(interface: &str) -> NetworkStatus {
    let mut status = snapshot::read(interface);
    probe::apply(&mut status);
    status
}
fn client_running() -> bool {
    CLIENT_RUNNING.load(Ordering::Relaxed)
}
fn ipv6_peer_seen() -> bool {
    IPV6_PEER.load(Ordering::Relaxed)
}
