//! Runtime PF ownership; no installer or persistent host configuration.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, Ipv6Addr},
    path::Path,
    process::{Command, Stdio},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const ANCHOR: &str = "com.apple/clankerd-networks";
const STATE: &str = "/var/run/clankerd-network-pf";
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    network_id: String,
    subnet: String,
    prefix: Option<String>,
    bridge: String,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    records: BTreeMap<String, Record>,
    token: Option<String>,
    #[serde(default)]
    acquiring: bool,
}
/// In-process policy changes; JSON is reserved for the persisted state file.
pub enum Change {
    Register {
        key: String,
        network_id: String,
        subnet: String,
        prefix: Option<Ipv6Addr>,
    },
    Remove {
        key: String,
    },
}
fn command(args: &[&str], text: Option<&str>) -> Result<String> {
    let mut child = Command::new(args[0])
        .args(&args[1..])
        .env("LC_ALL", "C")
        .stdin(if text.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let input = text.map(str::to_owned);
    let stdin = child.stdin.take();
    let writer = std::thread::spawn(move || -> std::io::Result<()> {
        if let (Some(mut stdin), Some(input)) = (stdin, input) {
            stdin.write_all(input.as_bytes())?;
        }
        Ok(())
    });
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let output = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).map(|_| bytes)
    });
    let errors = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).map(|_| bytes)
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut expired = false;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if std::time::Instant::now() >= deadline {
            expired = true;
            child.kill()?;
            break child.wait()?;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let written = writer.join().map_err(|_| "PF input worker failed")?;
    let stdout = output.join().map_err(|_| "PF output worker failed")??;
    let stderr = errors.join().map_err(|_| "PF error worker failed")??;
    if expired {
        return Err(format!(
            "{} exceeded 10-second deadline; outcome requires reconciliation",
            args.join(" ")
        )
        .into());
    }
    written?;
    let out = std::process::Output {
        status,
        stdout,
        stderr,
    };
    if !out.status.success() {
        return Err(format!(
            "{}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr)
        )
        .into());
    }
    Ok(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}
fn rules(records: &BTreeMap<String, Record>) -> String {
    let bridges: BTreeSet<_> = records.values().map(|r| &r.bridge).collect();
    let mut lines = String::new();
    for bridge in &bridges {
        lines +=
            &format!("anchor \"mark-{bridge}\" in on {bridge} all tag \"clankerd_{bridge}\"\n");
    }
    for source in &bridges {
        for target in &bridges {
            if source != target {
                lines += &format!(
                    "block drop out quick on {target} all tagged \"clankerd_{source}\" label \"clankerd-{source}-{target}\"\n"
                );
            }
        }
    }
    lines
}
fn bridge_for(subnet: &str) -> Result<String> {
    let (address, bits) = subnet.split_once('/').ok_or("expected subnet/prefix")?;
    let address: Ipv4Addr = address.parse()?;
    let bits: u32 = bits.parse()?;
    if !(8..=30).contains(&bits) {
        return Err("expected RFC1918 subnet /8 through /30".into());
    }
    let raw = u32::from(address);
    let [a, b, _, _] = address.octets();
    if raw & (u32::MAX >> bits) != 0
        || !(a == 10
            || (a == 172 && (16..32).contains(&b) && bits >= 12)
            || (a == 192 && b == 168 && bits >= 16))
    {
        return Err("expected aligned RFC1918 subnet".into());
    }
    let gateway = Ipv4Addr::from(raw + 1);
    for _ in 0..20 {
        let networks = sysinfo::Networks::new_with_refreshed_list();
        if let Some(bridge) = bridge_owning(
            networks
                .iter()
                .map(|(name, network)| (name.as_str(), network.ip_networks())),
            gateway,
        )? {
            return Ok(bridge.into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("no native bridge owns {gateway}").into())
}
fn bridge_owning<'a>(
    interfaces: impl IntoIterator<Item = (&'a str, &'a [sysinfo::IpNetwork])>,
    gateway: Ipv4Addr,
) -> Result<Option<&'a str>> {
    let mut bridges = interfaces.into_iter().filter(|(name, addresses)| {
        name.strip_prefix("bridge")
            .is_some_and(|suffix| !suffix.is_empty() && suffix.bytes().all(|b| b.is_ascii_digit()))
            && addresses.iter().any(|address| address.addr == gateway)
    });
    let bridge = bridges.next().map(|(name, _)| name);
    if bridges.next().is_some() {
        return Err(format!("multiple native bridges own {gateway}").into());
    }
    Ok(bridge)
}
fn persist(state: &State) -> Result<()> {
    let path = Path::new(STATE);
    fs::write(path.join("state.tmp"), serde_json::to_vec(state)?)?;
    fs::rename(path.join("state.tmp"), path.join("state.json"))?;
    Ok(())
}
fn release(token: &str) -> Result<()> {
    let token: u64 = token.parse()?;
    #[cfg(target_os = "macos")]
    {
        use std::os::fd::AsRawFd;
        #[repr(C)]
        struct RemoveToken {
            token: u64,
            references: u64,
        }
        let device = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/pf")?;
        let mut request = RemoveToken {
            token,
            references: 0,
        };
        let result = unsafe { libc::ioctl(device.as_raw_fd(), 0xc0104409, &mut request) };
        if result != 0 {
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.raw_os_error(),
                Some(libc::ENOENT) | Some(libc::EINVAL)
            ) {
                return Err(error.into());
            }
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = token;
        Err("PF release requires macOS".into())
    }
}
pub fn reset() -> Result<()> {
    let path = Path::new(STATE).join("state.json");
    if !path.exists() {
        return Ok(());
    }
    let state: State = serde_json::from_slice(&fs::read(&path)?)?;
    recover(
        state,
        || command(&["/sbin/pfctl", "-a", ANCHOR, "-F", "all"], None).map(|_| ()),
        release,
        persist,
    )
}
fn recover(
    state: State,
    flush: impl FnOnce() -> Result<()>,
    release: impl FnOnce(&str) -> Result<()>,
    save: impl FnOnce(&State) -> Result<()>,
) -> Result<()> {
    if state.acquiring {
        return Err("PF reference acquisition was interrupted; saved evidence requires administrator reconciliation".into());
    }
    flush()?;
    if let Some(token) = state.token {
        release(&token)?;
    }
    save(&State::default())
}

pub fn apply(uid: u32, change: Change) -> Result<()> {
    static LIVE: std::sync::Mutex<Option<State>> = std::sync::Mutex::new(None);
    let mut live = LIVE.lock().unwrap();
    let key = match &change {
        Change::Register { key, .. } | Change::Remove { key } => key,
    };
    fs::create_dir_all(STATE)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(STATE, fs::Permissions::from_mode(0o700))?;
    if live.is_none() {
        *live = Some(match fs::read(Path::new(STATE).join("state.json")) {
            Ok(b) => serde_json::from_slice(&b)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
            Err(e) => return Err(e.into()),
        });
    }
    let mut state = live.as_ref().unwrap().clone();
    if state.acquiring {
        return Err("PF reference acquisition needs administrator reconciliation".into());
    }
    let previous = state.records.clone();
    let record_key = format!("{uid}:{key}");
    if let Change::Register {
        network_id,
        subnet,
        prefix,
        ..
    } = change
    {
        if network_id.len() != 64
            || !network_id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err("invalid network identity".into());
        }
        if prefix
            .is_some_and(|p| p.octets()[0] != 0xfd || u128::from(p) & u128::from(u64::MAX) != 0)
        {
            return Err("expected aligned ULA /64".into());
        }
        let record = Record {
            network_id,
            bridge: bridge_for(&subnet)?,
            subnet,
            prefix: prefix.map(|p| format!("{p}/64")),
        };
        for (other_key, other) in &state.records {
            if other_key != &record_key
                && ((other.network_id == record.network_id) != (other.bridge == record.bridge)
                    || (other.bridge == record.bridge
                        && (other.subnet != record.subnet || other.prefix != record.prefix)))
            {
                return Err("native bridge conflicts with an active virtual network".into());
            }
        }
        state.records.insert(record_key, record);
    } else {
        state.records.remove(&record_key);
    }
    if !state.records.is_empty()
        && !command(&["/sbin/pfctl", "-sr"], None)?
            .lines()
            .any(|l| l.starts_with("anchor \"com.apple/*\""))
    {
        return Err("host PF rules must include anchor com.apple/*".into());
    }
    let content = rules(&state.records);
    command(
        &["/sbin/pfctl", "-n", "-a", ANCHOR, "-f", "-"],
        Some(&content),
    )?;
    let mut acquired = false;
    if !state.records.is_empty() && state.token.is_none() {
        state.acquiring = true;
        persist(&state)?;
        live.as_mut().unwrap().acquiring = true;
        let output = command(&["/sbin/pfctl", "-E"], None)?;
        state.token = output
            .lines()
            .find_map(|l| {
                l.trim()
                    .strip_prefix("Token : ")
                    .or_else(|| l.trim().strip_prefix("Token: "))
            })
            .map(str::to_string);
        if state
            .token
            .as_ref()
            .is_none_or(|t| t.is_empty() || !t.bytes().all(|b| b.is_ascii_digit()))
        {
            return Err("pfctl returned no reference token".into());
        }
        state.acquiring = false;
        persist(&state)?;
        live.as_mut().unwrap().token = state.token.clone();
        live.as_mut().unwrap().acquiring = false;
        acquired = true;
    }
    let applied = command(&["/sbin/pfctl", "-a", ANCHOR, "-f", "-"], Some(&content))
        .and_then(|_| persist(&state));
    if let Err(error) = applied {
        if let Err(rollback) = command(
            &["/sbin/pfctl", "-a", ANCHOR, "-f", "-"],
            Some(&rules(&previous)),
        ) {
            eprintln!(
                "clankerd-network: PF rollback failed: {rollback}; restart affected machines manually after administrator reconciliation"
            );
            std::process::exit(1);
        }
        if acquired {
            release(state.token.as_deref().unwrap())?;
            state.token = None;
        }
        state.records = previous;
        *live = Some(state.clone());
        persist(&state)?;
        return Err(error);
    }
    if state.records.is_empty()
        && let Some(token) = state.token.take()
    {
        release(&token)?;
        *live = Some(state.clone());
        persist(&state)?;
    }
    *live = Some(state);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_recovery_retains_saved_reference_and_a_later_attempt_completes() {
        let saved = std::cell::RefCell::new(State {
            token: Some("42".into()),
            ..State::default()
        });
        let result = recover(
            saved.borrow().clone(),
            || Ok(()),
            |_| Err("reference release failed".into()),
            |state| {
                *saved.borrow_mut() = state.clone();
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(saved.borrow().token.as_deref(), Some("42"));
        let state = saved.borrow().clone();
        recover(
            state,
            || Ok(()),
            |_| Ok(()),
            |state| {
                *saved.borrow_mut() = state.clone();
                Ok(())
            },
        )
        .unwrap();
        assert!(saved.borrow().token.is_none());
    }

    #[test]
    fn interrupted_acquisition_preserves_evidence_without_flushing_policy() {
        let state = State {
            acquiring: true,
            ..State::default()
        };
        assert!(
            recover(
                state,
                || panic!("must retain policy evidence"),
                |_| panic!("no known token"),
                |_| panic!("must retain saved evidence")
            )
            .is_err()
        );
    }

    fn record(bridge: &str) -> Record {
        Record {
            network_id: "a".repeat(64),
            subnet: "192.168.120.0/24".into(),
            prefix: Some("fd00:1::/64".into()),
            bridge: bridge.into(),
        }
    }

    #[test]
    fn bridge_lookup_requires_a_unique_native_bridge_with_the_exact_gateway_address() {
        let gateway = "192.168.120.1".parse().unwrap();
        let addresses = [
            "192.168.120.1/24".parse().unwrap(),
            "fd00::1/64".parse().unwrap(),
        ];
        let other = ["192.168.120.2/24".parse().unwrap()];
        let interfaces = [
            ("en0", addresses.as_slice()),
            ("bridge", addresses.as_slice()),
            ("bridgeX", addresses.as_slice()),
            ("bridge1", other.as_slice()),
            ("bridge2", addresses.as_slice()),
        ];
        assert_eq!(bridge_owning(interfaces, gateway).unwrap(), Some("bridge2"));
        assert_eq!(
            bridge_owning(interfaces[..4].iter().copied(), gateway).unwrap(),
            None
        );
        assert!(
            bridge_owning(
                [
                    ("bridge1", addresses.as_slice()),
                    ("bridge2", addresses.as_slice())
                ],
                gateway
            )
            .is_err()
        );
    }

    #[test]
    fn policy_marks_each_active_bridge_and_blocks_only_cross_bridge_egress() {
        let mut records = BTreeMap::new();
        assert_eq!(rules(&records), "");
        records.insert("example-a".into(), record("bridge1"));
        let one = "anchor \"mark-bridge1\" in on bridge1 all tag \"clankerd_bridge1\"\n";
        assert_eq!(rules(&records), one);
        records.insert("example-b".into(), record("bridge1"));
        assert_eq!(
            rules(&records),
            one,
            "same-network members share a policy boundary"
        );
        records.insert("example-c".into(), record("bridge2"));
        assert_eq!(
            rules(&records),
            concat!(
                "anchor \"mark-bridge1\" in on bridge1 all tag \"clankerd_bridge1\"\n",
                "anchor \"mark-bridge2\" in on bridge2 all tag \"clankerd_bridge2\"\n",
                "block drop out quick on bridge2 all tagged \"clankerd_bridge1\" label \"clankerd-bridge1-bridge2\"\n",
                "block drop out quick on bridge1 all tagged \"clankerd_bridge2\" label \"clankerd-bridge2-bridge1\"\n"
            )
        );
        records.remove("example-c");
        assert_eq!(
            rules(&records),
            one,
            "removal retains surviving members' policy"
        );
    }
}
