//! A minimal DHCPv4 client: DISCOVER, OFFER, REQUEST, ACK (RFC 2131) over an
//! abstract [`Transport`], so the protocol runs against a fake server in tests
//! and over an AF_PACKET socket in the guest (`packet`).
//!
//! Scope: one interface, one lease, no renewal. gvproxy leases for an hour
//! but never withdraws the address, and the address is installed without a
//! lifetime, so it stays valid for the machine's life.

use std::io;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

const MAGIC_COOKIE: [u8; 4] = [0x63, 0x82, 0x53, 0x63];
/// op, htype..giaddr, chaddr, sname, file, cookie.
const HEADER_LEN: usize = 240;
const BOOTREQUEST: u8 = 1;
const BOOTREPLY: u8 = 2;
const FLAG_BROADCAST: u16 = 0x8000;

const OPT_PAD: u8 = 0;
const OPT_MASK: u8 = 1;
const OPT_ROUTER: u8 = 3;
const OPT_DNS: u8 = 6;
const OPT_HOSTNAME: u8 = 12;
const OPT_DOMAIN: u8 = 15;
const OPT_MTU: u8 = 26;
const OPT_REQUESTED_IP: u8 = 50;
const OPT_LEASE_TIME: u8 = 51;
const OPT_MESSAGE_TYPE: u8 = 53;
const OPT_SERVER_ID: u8 = 54;
const OPT_PARAMETER_LIST: u8 = 55;
const OPT_SEARCH_LIST: u8 = 119;
const OPT_END: u8 = 255;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const ACK: u8 = 5;
const NAK: u8 = 6;

/// Moves DHCP payloads (the UDP payload, no IP or UDP headers).
pub trait Transport {
    /// Broadcasts `payload` to the DHCP server port.
    fn send(&mut self, payload: &[u8]) -> io::Result<()>;
    /// The next payload addressed to the client port, or `None` at `timeout`.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>>;
}

#[derive(Debug, Clone)]
pub struct Settings {
    /// Wait for the first answer; doubles per attempt up to `max_timeout`.
    pub initial_timeout: Duration,
    pub max_timeout: Duration,
    pub attempts: u32,
    /// Sent as option 12 when not empty.
    pub hostname: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            initial_timeout: Duration::from_secs(1),
            max_timeout: Duration::from_secs(8),
            attempts: 6,
            hostname: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Lease {
    pub address: Ipv4Addr,
    pub prefix_len: u8,
    pub router: Option<Ipv4Addr>,
    pub dns: Vec<Ipv4Addr>,
    pub search: Vec<String>,
    pub mtu: Option<u16>,
    pub lease_secs: Option<u32>,
    pub server: Ipv4Addr,
}

/// Runs the exchange until a lease is acknowledged or `settings.attempts`
/// rounds have passed.
pub fn acquire(
    transport: &mut dyn Transport,
    mac: [u8; 6],
    xid: u32,
    settings: &Settings,
) -> Result<Lease, String> {
    let mut timeout = settings.initial_timeout;
    for _ in 0..settings.attempts {
        let discover = build(DISCOVER, mac, xid, &[], settings);
        transport
            .send(&discover)
            .map_err(|e| format!("sending DISCOVER: {e}"))?;
        if let Some(offer) = wait_for(transport, mac, xid, timeout, &[OFFER])? {
            let server = offer.server_id.unwrap_or(Ipv4Addr::UNSPECIFIED);
            let extra = [
                (OPT_REQUESTED_IP, offer.yiaddr.octets().to_vec()),
                (OPT_SERVER_ID, server.octets().to_vec()),
            ];
            let request = build(REQUEST, mac, xid, &extra, settings);
            transport
                .send(&request)
                .map_err(|e| format!("sending REQUEST: {e}"))?;
            if let Some(reply) = wait_for(transport, mac, xid, timeout, &[ACK, NAK])?
                && reply.message_type == ACK
            {
                return Ok(reply.into_lease());
            }
        }
        timeout = (timeout * 2).min(settings.max_timeout);
    }
    Err(format!(
        "no DHCP lease after {} attempts (is the network backend running?)",
        settings.attempts
    ))
}

fn wait_for(
    transport: &mut dyn Transport,
    mac: [u8; 6],
    xid: u32,
    timeout: Duration,
    types: &[u8],
) -> Result<Option<Reply>, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Ok(None);
        }
        match transport.recv(left) {
            Err(e) => return Err(format!("receiving: {e}")),
            Ok(None) => return Ok(None),
            Ok(Some(bytes)) => {
                if let Some(reply) = Reply::parse(&bytes)
                    && reply.xid == xid
                    && reply.chaddr == mac
                    && types.contains(&reply.message_type)
                {
                    return Ok(Some(reply));
                }
            }
        }
    }
}

fn build(
    kind: u8,
    mac: [u8; 6],
    xid: u32,
    extra: &[(u8, Vec<u8>)],
    settings: &Settings,
) -> Vec<u8> {
    let mut m = vec![0u8; HEADER_LEN];
    m[0] = BOOTREQUEST;
    m[1] = 1; // Ethernet
    m[2] = 6;
    m[4..8].copy_from_slice(&xid.to_be_bytes());
    // Ask for a broadcast reply: the client has no address to unicast to yet.
    m[10..12].copy_from_slice(&FLAG_BROADCAST.to_be_bytes());
    m[28..34].copy_from_slice(&mac);
    m[236..240].copy_from_slice(&MAGIC_COOKIE);
    let mut option = |code: u8, value: &[u8]| {
        m.push(code);
        m.push(value.len() as u8);
        m.extend_from_slice(value);
    };
    option(OPT_MESSAGE_TYPE, &[kind]);
    for (code, value) in extra {
        option(*code, value);
    }
    if !settings.hostname.is_empty() {
        option(OPT_HOSTNAME, settings.hostname.as_bytes());
    }
    option(
        OPT_PARAMETER_LIST,
        &[
            OPT_MASK,
            OPT_ROUTER,
            OPT_DNS,
            OPT_DOMAIN,
            OPT_MTU,
            OPT_LEASE_TIME,
            OPT_SEARCH_LIST,
        ],
    );
    m.push(OPT_END);
    m
}

struct Reply {
    xid: u32,
    chaddr: [u8; 6],
    yiaddr: Ipv4Addr,
    message_type: u8,
    server_id: Option<Ipv4Addr>,
    mask: Option<Ipv4Addr>,
    router: Option<Ipv4Addr>,
    dns: Vec<Ipv4Addr>,
    domain: Option<String>,
    search: Vec<String>,
    mtu: Option<u16>,
    lease_secs: Option<u32>,
}

impl Reply {
    fn parse(b: &[u8]) -> Option<Reply> {
        if b.len() < HEADER_LEN || b[0] != BOOTREPLY || b[236..240] != MAGIC_COOKIE {
            return None;
        }
        let mut r = Reply {
            xid: u32::from_be_bytes(b[4..8].try_into().ok()?),
            chaddr: b[28..34].try_into().ok()?,
            yiaddr: ip(&b[16..20])?,
            message_type: 0,
            server_id: None,
            mask: None,
            router: None,
            dns: Vec::new(),
            domain: None,
            search: Vec::new(),
            mtu: None,
            lease_secs: None,
        };
        let mut i = HEADER_LEN;
        while i < b.len() {
            let code = b[i];
            if code == OPT_END {
                break;
            }
            if code == OPT_PAD {
                i += 1;
                continue;
            }
            let len = *b.get(i + 1)? as usize;
            let v = b.get(i + 2..i + 2 + len)?;
            match code {
                OPT_MESSAGE_TYPE if len == 1 => r.message_type = v[0],
                OPT_SERVER_ID => r.server_id = ip(v),
                OPT_MASK => r.mask = ip(v),
                OPT_ROUTER => r.router = ip(v.get(..4)?),
                OPT_DNS => {
                    r.dns = v
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|c| Ipv4Addr::from(*c))
                        .collect()
                }
                OPT_DOMAIN => {
                    r.domain = Some(String::from_utf8_lossy(v).trim_end_matches('\0').to_owned())
                }
                OPT_MTU if len == 2 => r.mtu = Some(u16::from_be_bytes([v[0], v[1]])),
                OPT_LEASE_TIME if len == 4 => {
                    r.lease_secs = Some(u32::from_be_bytes(v.try_into().ok()?))
                }
                OPT_SEARCH_LIST => r.search = parse_labels(v),
                _ => {}
            }
            i += 2 + len;
        }
        (r.message_type != 0).then_some(r)
    }

    fn into_lease(self) -> Lease {
        let prefix_len = self
            .mask
            .map(|m| u32::from(m).leading_ones() as u8)
            .unwrap_or(24);
        let mut search = self.search;
        if search.is_empty()
            && let Some(domain) = self.domain.filter(|d| !d.is_empty())
        {
            search.push(domain);
        }
        Lease {
            address: self.yiaddr,
            prefix_len,
            router: self.router,
            dns: self.dns,
            search,
            mtu: self.mtu.filter(|m| *m >= 576),
            lease_secs: self.lease_secs,
            server: self.server_id.unwrap_or(Ipv4Addr::UNSPECIFIED),
        }
    }
}

fn ip(b: &[u8]) -> Option<Ipv4Addr> {
    let o: [u8; 4] = b.try_into().ok()?;
    Some(Ipv4Addr::from(o))
}

/// RFC 1035 label sequences (option 119), without compression pointers.
fn parse_labels(mut v: &[u8]) -> Vec<String> {
    let mut names = Vec::new();
    let mut current: Vec<String> = Vec::new();
    while let Some((&len, rest)) = v.split_first() {
        if len == 0 {
            if !current.is_empty() {
                names.push(current.join("."));
                current.clear();
            }
            v = rest;
        } else if len & 0xc0 != 0 || rest.len() < len as usize {
            break;
        } else {
            current.push(String::from_utf8_lossy(&rest[..len as usize]).into_owned());
            v = &rest[len as usize..];
        }
    }
    names
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::sync::mpsc;

    const MAC: [u8; 6] = [0x5a, 0x94, 0xef, 0xe4, 0x0c, 0xee];

    /// Client side over loopback UDP: "broadcast" goes to the fake server.
    struct Udp {
        socket: UdpSocket,
        server: std::net::SocketAddr,
    }

    impl Transport for Udp {
        fn send(&mut self, payload: &[u8]) -> io::Result<()> {
            self.socket.send_to(payload, self.server).map(|_| ())
        }
        fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
            self.socket.set_read_timeout(Some(timeout))?;
            let mut buf = [0u8; 1500];
            match self.socket.recv(&mut buf) {
                Ok(n) => Ok(Some(buf[..n].to_vec())),
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(None)
                }
                Err(e) => Err(e),
            }
        }
    }

    /// What the fake server does with the n-th client message.
    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        Normal,
        /// Ignore the first DISCOVER.
        DropFirstDiscover,
        /// NAK the first REQUEST.
        NakFirstRequest,
        /// Send a reply with the wrong xid before every real one.
        NoiseFirst,
        Silent,
    }

    /// Message type (option 53) of a client message, read by hand.
    fn kind(m: &[u8]) -> u8 {
        let mut i = 240;
        while m[i] != 255 {
            if m[i] == 53 {
                return m[i + 2];
            }
            i += 2 + m[i + 1] as usize;
        }
        panic!("no message type")
    }

    /// A reply laid out byte by byte like gvproxy's (insomniacslk/dhcp) ones.
    fn reply(kind: u8, xid: [u8; 4], mac: [u8; 6]) -> Vec<u8> {
        let mut m = vec![0u8; 240];
        m[0] = 2;
        m[1] = 1;
        m[2] = 6;
        m[4..8].copy_from_slice(&xid);
        m[16..20].copy_from_slice(&[192, 168, 127, 2]);
        m[28..34].copy_from_slice(&mac);
        m[236..240].copy_from_slice(&[0x63, 0x82, 0x53, 0x63]);
        m.extend_from_slice(&[53, 1, kind]);
        if kind != 6 {
            m.extend_from_slice(&[54, 4, 192, 168, 127, 1]);
            m.extend_from_slice(&[51, 4, 0, 0, 0x0e, 0x10]); // 3600 s
            m.extend_from_slice(&[1, 4, 255, 255, 255, 0]);
            m.extend_from_slice(&[3, 4, 192, 168, 127, 1]);
            m.extend_from_slice(&[6, 8, 192, 168, 127, 1, 1, 1, 1, 1]);
            m.extend_from_slice(&[26, 2, 0x05, 0xdc]); // 1500
            // search list: "corp.example" and "lan"
            let labels: &[u8] = b"\x04corp\x07example\x00\x03lan\x00";
            m.push(119);
            m.push(labels.len() as u8);
            m.extend_from_slice(labels);
        }
        m.push(255);
        m
    }

    struct Fake {
        client: Udp,
        /// Messages the server saw, by type.
        seen: mpsc::Receiver<Vec<u8>>,
    }

    fn start(mode: Mode) -> Fake {
        let server = UdpSocket::bind("127.0.0.1:0").unwrap();
        let client = UdpSocket::bind("127.0.0.1:0").unwrap();
        let (server_addr, client_addr) =
            (server.local_addr().unwrap(), client.local_addr().unwrap());
        let (tx, seen) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut dropped, mut naked) = (false, false);
            let mut buf = [0u8; 1500];
            while let Ok(n) = server.recv(&mut buf) {
                let m = buf[..n].to_vec();
                tx.send(m.clone()).unwrap();
                let (k, xid) = (kind(&m), <[u8; 4]>::try_from(&m[4..8]).unwrap());
                let mac = <[u8; 6]>::try_from(&m[28..34]).unwrap();
                let send = |bytes: Vec<u8>| {
                    server.send_to(&bytes, client_addr).unwrap();
                };
                if mode == Mode::NoiseFirst {
                    send(reply(if k == 1 { 2 } else { 5 }, [9, 9, 9, 9], mac));
                }
                match (mode, k) {
                    (Mode::Silent, _) => {}
                    (Mode::DropFirstDiscover, 1) if !dropped => dropped = true,
                    (Mode::NakFirstRequest, 3) if !naked => {
                        naked = true;
                        send(reply(NAK, xid, mac));
                    }
                    (_, 1) => send(reply(OFFER, xid, mac)),
                    (_, 3) => send(reply(ACK, xid, mac)),
                    _ => {}
                }
            }
        });
        Fake {
            client: Udp {
                socket: client,
                server: server_addr,
            },
            seen,
        }
    }

    fn fast() -> Settings {
        Settings {
            initial_timeout: Duration::from_millis(150),
            max_timeout: Duration::from_millis(300),
            attempts: 4,
            hostname: "box".into(),
        }
    }

    fn expected() -> Lease {
        Lease {
            address: Ipv4Addr::new(192, 168, 127, 2),
            prefix_len: 24,
            router: Some(Ipv4Addr::new(192, 168, 127, 1)),
            dns: vec![Ipv4Addr::new(192, 168, 127, 1), Ipv4Addr::new(1, 1, 1, 1)],
            search: vec!["corp.example".into(), "lan".into()],
            mtu: Some(1500),
            lease_secs: Some(3600),
            server: Ipv4Addr::new(192, 168, 127, 1),
        }
    }

    #[test]
    fn a_full_exchange_yields_the_leased_configuration() {
        let mut fake = start(Mode::Normal);
        let lease = acquire(&mut fake.client, MAC, 0x1234_5678, &fast()).unwrap();
        assert_eq!(lease, expected());
    }

    #[test]
    fn the_client_speaks_rfc_2131_on_the_wire() {
        let mut fake = start(Mode::Normal);
        acquire(&mut fake.client, MAC, 0x1234_5678, &fast()).unwrap();
        let discover = fake.seen.recv().unwrap();
        let request = fake.seen.recv().unwrap();

        assert_eq!(kind(&discover), 1);
        assert_eq!(discover[0], 1, "BOOTREQUEST");
        assert_eq!(&discover[4..8], &[0x12, 0x34, 0x56, 0x78], "xid");
        assert_eq!(&discover[10..12], &[0x80, 0x00], "broadcast flag");
        assert_eq!(&discover[28..34], &MAC, "chaddr");
        assert_eq!(
            &discover[236..240],
            &[0x63, 0x82, 0x53, 0x63],
            "magic cookie"
        );
        assert_eq!(*discover.last().unwrap(), 255, "end option");
        // Parameter request list asks for mask, router, dns, domain, mtu, lease, search.
        let hay = &discover[240..];
        assert!(
            hay.windows(9)
                .any(|w| w == [55, 7, 1, 3, 6, 15, 26, 51, 119])
        );
        assert!(hay.windows(5).any(|w| w == [12, 3, b'b', b'o', b'x']));

        assert_eq!(kind(&request), 3);
        let hay = &request[240..];
        assert!(
            hay.windows(6).any(|w| w == [50, 4, 192, 168, 127, 2]),
            "requested ip"
        );
        assert!(
            hay.windows(6).any(|w| w == [54, 4, 192, 168, 127, 1]),
            "server id"
        );
    }

    #[test]
    fn a_lost_discover_is_retransmitted() {
        let mut fake = start(Mode::DropFirstDiscover);
        let lease = acquire(&mut fake.client, MAC, 7, &fast()).unwrap();
        assert_eq!(lease.address, Ipv4Addr::new(192, 168, 127, 2));
    }

    #[test]
    fn a_nak_restarts_the_exchange() {
        let mut fake = start(Mode::NakFirstRequest);
        let lease = acquire(&mut fake.client, MAC, 7, &fast()).unwrap();
        assert_eq!(lease.address, Ipv4Addr::new(192, 168, 127, 2));
        let kinds: Vec<u8> = fake.seen.try_iter().map(|m| kind(&m)).collect();
        assert_eq!(kinds, [1, 3, 1, 3]);
    }

    #[test]
    fn replies_for_another_transaction_are_ignored() {
        let mut fake = start(Mode::NoiseFirst);
        let lease = acquire(&mut fake.client, MAC, 0xdead_beef, &fast()).unwrap();
        assert_eq!(lease, expected());
    }

    #[test]
    fn no_server_means_a_clear_error() {
        let mut fake = start(Mode::Silent);
        let err = acquire(&mut fake.client, MAC, 1, &fast()).unwrap_err();
        assert!(err.contains("no DHCP lease"), "{err}");
    }

    #[test]
    fn the_prefix_length_follows_the_leased_mask() {
        // A /16 mask.
        let mut m = reply(ACK, [0; 4], MAC);
        let at = m
            .windows(6)
            .position(|w| w == [1, 4, 255, 255, 255, 0])
            .unwrap();
        m[at + 4] = 0;
        m[at + 5] = 0;
        let lease = Reply::parse(&m).unwrap().into_lease();
        assert_eq!(lease.prefix_len, 16);
    }
}
