//! Independently observe a numeric TCP endpoint for each family. Probes never
//! select an application family, gate startup, or assert universal reachability.
use clankerd_proto::guest::{
    ExternalConnectivity, NetworkProbeEndpoints, NetworkProbeObservation, NetworkStatus,
};
use std::net::TcpStream;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(2);
const INTERVAL: Duration = Duration::from_secs(30);
const POLL: Duration = Duration::from_secs(1);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static RESULTS: Mutex<[Option<(String, NetworkProbeObservation)>; 2]> = Mutex::new([None, None]);

pub fn start(endpoints: NetworkProbeEndpoints) {
    start_on(None, endpoints);
}

pub fn start_network(interface: String, endpoints: NetworkProbeEndpoints) {
    start_on(Some(interface), endpoints);
}

fn start_on(interface: Option<String>, endpoints: NetworkProbeEndpoints) {
    for (index, destination) in [endpoints.ipv4, endpoints.ipv6].into_iter().enumerate() {
        let interface = interface.clone();
        std::thread::spawn(move || {
            let mut previous = String::new();
            let mut next = std::time::Instant::now();
            loop {
                let key = interface
                    .as_ref()
                    .map(|interface| configuration(&super::snapshot::read(interface), index))
                    .unwrap_or_default();
                if key == previous && std::time::Instant::now() < next {
                    std::thread::sleep(POLL);
                    continue;
                }
                previous = key.clone();
                next = std::time::Instant::now() + INTERVAL;
                let mapped_ipv4 = matches!(destination.ip(), std::net::IpAddr::V6(address) if address.to_ipv4_mapped().is_some());
                let result = if destination.is_ipv4() == (index == 0) && !mapped_ipv4 {
                    TcpStream::connect_timeout(&destination, TIMEOUT).map(|_| ())
                } else {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "destination belongs to the wrong IP family",
                    ))
                };
                let checked_at_unix_seconds = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                let diagnostic = match &result {
                    Ok(()) => format!(
                        "TCP connection to {destination} succeeded; other destinations are unverified"
                    ),
                    Err(error) => format!(
                        "TCP connection to {destination} failed: {error}; other destinations are unverified"
                    ),
                };
                RESULTS.lock().unwrap_or_else(|error| error.into_inner())[index] = Some((
                    key,
                    NetworkProbeObservation {
                        destination,
                        connected: result.is_ok(),
                        checked_at_unix_seconds,
                        diagnostic,
                    },
                ));
                std::thread::sleep(POLL);
            }
        });
    }
}

fn configuration(status: &NetworkStatus, index: usize) -> String {
    let v4 = index == 0;
    let family = if v4 { &status.ipv4 } else { &status.ipv6 };
    serde_json::to_string(&(
        status
            .address_details
            .iter()
            .filter(|a| a.address.is_ipv4() == v4)
            .collect::<Vec<_>>(),
        status
            .routes
            .iter()
            .filter(|r| r.destination.is_ipv4() == v4)
            .collect::<Vec<_>>(),
        &status.nameservers,
        family.local_available,
        family.default_route,
        super::client_running(),
        GENERATION.load(Ordering::Relaxed),
    ))
    .unwrap_or_default()
}

pub fn invalidate() {
    GENERATION.fetch_add(1, Ordering::Relaxed);
    *RESULTS.lock().unwrap_or_else(|error| error.into_inner()) = [None, None];
}

pub fn apply(status: &mut NetworkStatus) {
    let keys = [configuration(status, 0), configuration(status, 1)];
    let mut results = RESULTS.lock().unwrap_or_else(|error| error.into_inner());
    for (index, family) in [&mut status.ipv4, &mut status.ipv6].into_iter().enumerate() {
        if results[index]
            .as_ref()
            .is_some_and(|(key, _)| !key.is_empty() && *key != keys[index])
        {
            results[index] = None;
        }
        if let Some((key, observation)) = &results[index] {
            if !key.is_empty() && !family.local_available {
                family.external = ExternalConnectivity::Unavailable;
                continue;
            }
            family.external = if observation.connected {
                ExternalConnectivity::Available
            } else {
                ExternalConnectivity::Unavailable
            };
            family.probe = Some(observation.clone());
        }
    }
}
