//! Binary dhcpcd hook. No OCI shell, resolvconf, hostname or mDNS hooks.
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::net::{IpAddr, Ipv6Addr};
use std::os::fd::AsRawFd;
use std::path::Path;

use serde::{Deserialize, Serialize};

const STATE: &str = "/run/clankerd/network";

#[derive(Default, Serialize, Deserialize)]
struct Contribution {
    servers: Vec<TimedValue>,
    search: Vec<TimedValue>,
    evidence: bool,
}
#[derive(Serialize, Deserialize)]
struct TimedValue {
    value: String,
    expires: Option<u64>,
}

pub fn evidence(source: &str) -> bool {
    fs::read(Path::new(STATE).join(format!("{source}.json")))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Contribution>(&bytes).ok())
        .is_some_and(|data| data.evidence)
}

pub fn run() -> io::Result<()> {
    let env: HashMap<String, String> = std::env::vars().collect();
    let interface = env.get("interface").map(String::as_str).unwrap_or("");
    if interface != "eth0" {
        return Ok(());
    }
    let reason = env.get("reason").map(String::as_str).unwrap_or("");
    let state = Path::new(STATE);
    let _lock = lock(state)?;
    if matches!(
        reason,
        "NOCARRIER" | "NOCARRIER_ROAMING" | "STOPPED" | "DEPARTED"
    ) {
        for source in ["dhcp", "dhcp6", "ra"] {
            let _ = fs::remove_file(state.join(format!("{source}.json")));
        }
        return refresh_locked();
    }
    let source = match env.get("protocol").map(String::as_str) {
        Some("dhcp") => "dhcp",
        Some("dhcp6") => "dhcp6",
        Some("ra") => "ra",
        _ => return Ok(()),
    };
    let path = state.join(format!("{source}.json"));
    if matches!(
        reason,
        "EXPIRE" | "EXPIRE6" | "RELEASE" | "RELEASE6" | "NAK" | "STOP" | "STOP6"
    ) {
        let _ = fs::remove_file(path);
        return refresh_locked();
    }
    let mut data = Contribution::default();
    match source {
        "dhcp" => {
            data.evidence = matches!(reason, "BOUND" | "RENEW" | "REBIND" | "REBOOT" | "INFORM");
            add_servers(&mut data, &env, "new_domain_name_servers", interface, None);
            add_search(&mut data, &env, "new_domain_search", None);
            if data.search.is_empty() {
                add_search(&mut data, &env, "new_domain_name", None);
            }
        }
        "dhcp6" => {
            data.evidence = matches!(
                reason,
                "BOUND6" | "RENEW6" | "REBIND6" | "REBOOT6" | "INFORM6"
            );
            add_servers(&mut data, &env, "new_dhcp6_name_servers", interface, None);
            add_search(&mut data, &env, "new_dhcp6_domain_search", None);
        }
        _ => {
            data.evidence = env.contains_key("nd1_acquired");
            for i in 1..=128 {
                let Some(acquired) = number(&env, &format!("nd{i}_acquired")) else {
                    break;
                };
                let Some(now) = number(&env, &format!("nd{i}_now")) else {
                    continue;
                };
                for j in 1..=128 {
                    let rdnss = format!("nd{i}_rdnss{j}");
                    let dnssl = format!("nd{i}_dnssl{j}");
                    let rlife = number(&env, &format!("{rdnss}_lifetime"));
                    let dlife = number(&env, &format!("{dnssl}_lifetime"));
                    if rlife.is_none() && dlife.is_none() {
                        break;
                    }
                    if let Some(remaining) = rlife
                        .and_then(|life| life.checked_sub(now.saturating_sub(acquired)))
                        .filter(|&remaining| remaining > 0)
                    {
                        add_servers(
                            &mut data,
                            &env,
                            &format!("{rdnss}_servers"),
                            interface,
                            Some(monotonic_secs() + remaining),
                        );
                    }
                    if let Some(remaining) = dlife
                        .and_then(|life| life.checked_sub(now.saturating_sub(acquired)))
                        .filter(|&remaining| remaining > 0)
                    {
                        add_search(
                            &mut data,
                            &env,
                            &format!("{dnssl}_search"),
                            Some(monotonic_secs() + remaining),
                        );
                    }
                }
            }
        }
    }
    let temporary = state.join(format!("{source}.{}.tmp", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(&data)?)?;
    fs::rename(temporary, path)?;
    refresh_locked()
}

fn number(env: &HashMap<String, String>, key: &str) -> Option<u64> {
    env.get(key)?.parse().ok()
}
fn add_servers(
    data: &mut Contribution,
    env: &HashMap<String, String>,
    key: &str,
    interface: &str,
    expires: Option<u64>,
) {
    if let Some(text) = env.get(key) {
        for server in text.split_whitespace() {
            let bare = server.split('%').next().unwrap_or("");
            if let Ok(address) = bare.parse::<IpAddr>() {
                let value = match address {
                    IpAddr::V6(v6) if link_local(v6) => format!("{v6}%{interface}"),
                    _ => address.to_string(),
                };
                data.servers.push(TimedValue { value, expires });
            }
        }
    }
}
fn add_search(
    data: &mut Contribution,
    env: &HashMap<String, String>,
    key: &str,
    expires: Option<u64>,
) {
    if let Some(text) = env.get(key) {
        for domain in text.split_whitespace().filter(|domain| {
            domain.len() <= 253
                && domain
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'.' || byte == b'-')
        }) {
            data.search.push(TimedValue {
                value: domain.into(),
                expires,
            });
        }
    }
}
fn link_local(address: Ipv6Addr) -> bool {
    address.segments()[0] & 0xffc0 == 0xfe80
}
fn monotonic_secs() -> u64 {
    // SAFETY: valid timespec storage; CLOCK_MONOTONIC cannot jump on clock setup.
    unsafe {
        let mut time: libc::timespec = std::mem::zeroed();
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time);
        time.tv_sec.max(0) as u64
    }
}

pub fn refresh() -> io::Result<()> {
    let _lock = lock(Path::new(STATE))?;
    refresh_locked()
}

// The hook runs in separate processes while PID1 expires DNS every second.
// Hold one lock across contribution changes and publication so older snapshots
// cannot overwrite a newer lease/RA event or collide on the PID1 temp file.
fn lock(state: &Path) -> io::Result<File> {
    fs::create_dir_all(state)?;
    let file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("resolver.lock"))?;
    loop {
        // SAFETY: file owns a live descriptor. Closing it releases LOCK_EX.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0 {
            return Ok(file);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

fn refresh_locked() -> io::Result<()> {
    let state = Path::new(STATE);
    let mut servers = Vec::new();
    let mut search = Vec::new();
    let now = monotonic_secs();
    for source in ["dhcp", "dhcp6", "ra"] {
        let path = state.join(format!("{source}.json"));
        let Some(data) = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Contribution>(&bytes).ok())
        else {
            continue;
        };
        for (values, out) in [(data.servers, &mut servers), (data.search, &mut search)] {
            for value in values {
                if value.expires.is_none_or(|expires| expires > now) && !out.contains(&value.value)
                {
                    out.push(value.value);
                }
            }
        }
    }
    let result = super::files::write_resolv_conf(Path::new("/"), &servers, &search);
    match &result {
        Ok(()) => {
            fs::write(state.join("resolver-ready"), b"")?;
            let _ = fs::remove_file(state.join("resolver-error"));
        }
        Err(error) => {
            let _ = fs::remove_file(state.join("resolver-ready"));
            let _ = fs::write(state.join("resolver-error"), error.to_string());
        }
    }
    result
}

pub fn resolver_healthy() -> bool {
    Path::new(STATE).join("resolver-ready").exists()
}
