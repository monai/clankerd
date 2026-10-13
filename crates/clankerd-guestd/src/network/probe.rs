//! Independently observe a numeric TCP endpoint for each family. Probes never
//! select an application family, gate startup, or assert universal reachability.
use clankerd_proto::guest::{
    ExternalConnectivity, NetworkProbeEndpoints, NetworkProbeObservation, NetworkStatus,
};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const TIMEOUT: Duration = Duration::from_secs(2);
const INTERVAL: Duration = Duration::from_secs(30);
static RESULTS: Mutex<[Option<NetworkProbeObservation>; 2]> = Mutex::new([None, None]);

pub fn start(endpoints: NetworkProbeEndpoints) {
    for (index, destination) in [endpoints.ipv4, endpoints.ipv6].into_iter().enumerate() {
        std::thread::spawn(move || {
            loop {
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
                RESULTS.lock().unwrap_or_else(|error| error.into_inner())[index] =
                    Some(NetworkProbeObservation {
                        destination,
                        connected: result.is_ok(),
                        checked_at_unix_seconds,
                        diagnostic,
                    });
                std::thread::sleep(INTERVAL);
            }
        });
    }
}

pub fn apply(status: &mut NetworkStatus) {
    let results = RESULTS.lock().unwrap_or_else(|error| error.into_inner());
    for (family, result) in [
        (&mut status.ipv4, &results[0]),
        (&mut status.ipv6, &results[1]),
    ] {
        if let Some(observation) = result {
            family.external = if observation.connected {
                ExternalConnectivity::Available
            } else {
                ExternalConnectivity::Unavailable
            };
            family.probe = Some(observation.clone());
        }
    }
}
