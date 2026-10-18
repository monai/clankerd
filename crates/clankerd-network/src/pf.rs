//! Runtime PF ownership; no installer or persistent host configuration.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File},
    io::{Read, Write},
    net::{Ipv4Addr, Ipv6Addr},
    os::fd::AsRawFd,
    path::Path,
    process::{Command, Stdio},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const ANCHOR: &str = "com.apple/clankerd-networks";
const STATE: &str = "/var/run/clankerd-network-pf";
#[derive(Clone, Serialize, Deserialize)]
struct Record {
    uid: u32,
    socket: String,
    pid: u32,
    network_id: String,
    subnet: String,
    prefix: Option<String>,
    bridge: String,
}
#[derive(Default, Serialize, Deserialize)]
struct State {
    records: BTreeMap<String, Record>,
    token: Option<String>,
}
#[derive(Deserialize)]
struct Request {
    action: String,
    socket: String,
    pid: Option<u32>,
    network_id: Option<String>,
    subnet: Option<String>,
    prefix: Option<Ipv6Addr>,
}
fn command(args: &[&str], text: Option<&str>) -> Result<String> {
    let mut child = Command::new(args[0])
        .args(&args[1..])
        .stdin(if text.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(text) = text {
        child.stdin.take().unwrap().write_all(text.as_bytes())?;
    }
    let out = child.wait_with_output()?;
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
fn alive(record: &Record) -> bool {
    let Ok(out) = command(
        &[
            "/bin/ps",
            "-o",
            "uid=,command=",
            "-p",
            &record.pid.to_string(),
        ],
        None,
    ) else {
        return false;
    };
    let fields: Vec<_> = out.split_whitespace().collect();
    fields.len() >= 4
        && fields[0].parse::<u32>().ok() == Some(record.uid)
        && fields[1].ends_with("/clankerd-network")
        && fields[2..]
            .windows(2)
            .any(|pair| pair == ["--socket", &record.socket])
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
    let gateway = Ipv4Addr::from(raw + 1).to_string();
    for _ in 0..20 {
        let route = command(&["/sbin/route", "-n", "get", &gateway], None)?;
        if let Some(bridge) = route
            .lines()
            .find_map(|l| l.trim().strip_prefix("interface: "))
            && bridge.starts_with("bridge")
            && !bridge[6..].is_empty()
            && bridge[6..].bytes().all(|b| b.is_ascii_digit())
        {
            let addresses = command(&["/sbin/ifconfig", bridge], None)?;
            if addresses.lines().any(|l| {
                let mut f = l.split_whitespace();
                f.next() == Some("inet") && f.next() == Some(&gateway)
            }) {
                return Ok(bridge.into());
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    Err(format!("no native bridge owns {gateway}").into())
}
fn persist(state: &State) -> Result<()> {
    let path = Path::new(STATE);
    fs::write(path.join("state.tmp"), serde_json::to_vec(state)?)?;
    fs::rename(path.join("state.tmp"), path.join("state.json"))?;
    Ok(())
}
pub fn run() -> Result<()> {
    // SAFETY: reads effective identity only.
    if !cfg!(target_os = "macos") || unsafe { libc::geteuid() } != 0 {
        return Err("PF requires administrator authorization on macOS".into());
    }
    let uid: u32 = std::env::var("SUDO_UID")?.parse()?;
    let mut input = String::new();
    std::io::stdin().take(4097).read_to_string(&mut input)?;
    if input.len() > 4096 {
        return Err("PF request too large".into());
    }
    let request: Request = serde_json::from_str(&input)?;
    if !["register", "remove"].contains(&request.action.as_str())
        || !request.socket.starts_with('/')
        || request.socket.len() > 1024
    {
        return Err("invalid PF request".into());
    }
    fs::create_dir_all(STATE)?;
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(STATE, fs::Permissions::from_mode(0o700))?;
    let lock = File::options()
        .create(true)
        .append(true)
        .open(Path::new(STATE).join("lock"))?;
    // SAFETY: lock fd is open and owned until the transaction completes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let mut state: State = match fs::read(Path::new(STATE).join("state.json")) {
        Ok(b) => serde_json::from_slice(&b)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => State::default(),
        Err(e) => return Err(e.into()),
    };
    let previous = state.records.clone();
    state.records.retain(|_, r| alive(r));
    let key = Sha256::digest(format!("{uid}:{}", request.socket))
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if request.action == "register" {
        let network_id = request.network_id.ok_or("missing network identity")?;
        if network_id.len() != 64
            || !network_id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err("invalid network identity".into());
        }
        if request
            .prefix
            .is_some_and(|p| p.octets()[0] != 0xfd || u128::from(p) & u128::from(u64::MAX) != 0)
        {
            return Err("expected aligned ULA /64".into());
        }
        let subnet = request.subnet.ok_or("missing subnet")?;
        let record = Record {
            uid,
            socket: request.socket,
            pid: request.pid.filter(|p| *p > 1).ok_or("missing PID")?,
            network_id,
            bridge: bridge_for(&subnet)?,
            subnet,
            prefix: request.prefix.map(|p| format!("{p}/64")),
        };
        if !alive(&record) {
            return Err("attachment does not belong to caller and socket".into());
        }
        for (other_key, other) in &state.records {
            if other_key != &key
                && ((other.network_id == record.network_id) != (other.bridge == record.bridge)
                    || (other.bridge == record.bridge
                        && (other.subnet != record.subnet || other.prefix != record.prefix)))
            {
                return Err("native bridge conflicts with an active virtual network".into());
            }
        }
        state.records.insert(key, record);
    } else {
        state.records.remove(&key);
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
        acquired = true;
    }
    let applied = command(&["/sbin/pfctl", "-a", ANCHOR, "-f", "-"], Some(&content))
        .and_then(|_| persist(&state));
    if let Err(error) = applied {
        let _ = command(
            &["/sbin/pfctl", "-a", ANCHOR, "-f", "-"],
            Some(&rules(&previous)),
        );
        if acquired {
            let _ = command(
                &["/sbin/pfctl", "-X", state.token.as_deref().unwrap()],
                None,
            );
        }
        return Err(error);
    }
    if state.records.is_empty()
        && let Some(token) = state.token.take()
    {
        command(&["/sbin/pfctl", "-X", &token], None)?;
        persist(&state)?;
    }
    println!("{}", serde_json::json!({"attachments":state.records.len()}));
    Ok(())
}
