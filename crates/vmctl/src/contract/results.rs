//! Public result records; persistence records are mapped into these explicitly.
use libclankerd::{
    BindSemantics, Error, ImageInfo, MachineInfo, Mount, VirtualNetwork, VolumeInfo,
};
use schemars::JsonSchema;
use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, JsonSchema)]
pub struct Diagnostic {
    pub origin: &'static str,
    pub target: Option<String>,
    pub kind: String,
    pub message: String,
}
#[derive(Serialize, JsonSchema)]
pub struct Operation {
    pub target: String,
    pub id: Option<String>,
    pub success: bool,
    pub error: Option<Diagnostic>,
}
#[derive(Serialize, JsonSchema)]
pub struct MachineRecord {
    pub id: String,
    pub name: String,
    /// Milliseconds since Unix epoch (UTC).
    pub created: u64,
    pub image_id: String,
    pub config: ConfigRecord,
    pub host_config: HostRecord,
    pub state: StateRecord,
    pub network: Option<NetworkStatusRecord>,
}
#[derive(Serialize, JsonSchema)]
pub struct ConfigRecord {
    pub image: String,
    pub entrypoint: Vec<String>,
    pub cmd: Vec<String>,
    pub env: Vec<String>,
    pub user: String,
    pub working_dir: String,
    pub tty: bool,
    pub open_stdin: bool,
    pub stop_signal: String,
}
#[derive(Serialize, JsonSchema)]
pub struct HostRecord {
    pub virtual_network: Option<String>,
    pub network_mode: String,
    pub bridge_interface: Option<String>,
    pub shared_network: SubnetRecord,
    pub shared_ipv6_prefix: Option<String>,
    pub cpus: Option<u32>,
    pub memory: Option<u64>,
    pub mounts: Vec<MountRecord>,
    pub socket_bindings: Vec<SocketRecord>,
    pub restart_policy: String,
    pub pull_policy: String,
}
#[derive(Serialize, JsonSchema)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MountRecord {
    Bind {
        source: String,
        target: String,
        read_only: bool,
        semantics: BindSemantics,
    },
    Volume {
        name: String,
        target: String,
    },
}
#[derive(Serialize, JsonSchema)]
pub struct SocketRecord {
    pub host_path: String,
    pub guest_path: String,
}
#[derive(Serialize, JsonSchema)]
pub struct StateRecord {
    pub status: String,
    pub exit_code: i32,
    pub oom_killed: bool,
    pub error: String,
    pub pid: Option<u32>,
    pub started_at: Option<u64>,
    pub finished_at: Option<u64>,
    pub restart_count: u32,
}
#[derive(Serialize, JsonSchema)]
pub struct NetworkStatusRecord {
    pub addresses: Vec<String>,
    pub address_details: Vec<AddressRecord>,
    pub routes: Vec<RouteRecord>,
    pub nameservers: Vec<String>,
    pub search: Vec<String>,
    pub ipv4: FamilyRecord,
    pub ipv6: FamilyRecord,
    pub ready: bool,
    pub diagnostic: String,
}
#[derive(Serialize, JsonSchema)]
pub struct AddressRecord {
    pub address: String,
    pub prefix_len: u8,
    pub interface: String,
    pub scope_id: u32,
    pub tentative: bool,
    pub dad_failed: bool,
    pub deprecated: bool,
}
#[derive(Serialize, JsonSchema)]
pub struct RouteRecord {
    pub destination: String,
    pub prefix_len: u8,
    pub gateway: Option<String>,
    pub interface: String,
    pub metric: u32,
}
#[derive(Serialize, JsonSchema)]
pub struct FamilyRecord {
    pub local_available: bool,
    pub default_route: bool,
    pub external: String,
    pub probe: Option<ProbeRecord>,
}
#[derive(Serialize, JsonSchema)]
pub struct ProbeRecord {
    pub destination: String,
    pub connected: bool,
    /// Milliseconds since Unix epoch (UTC).
    pub checked_at: u64,
    pub diagnostic: String,
}
impl From<clankerd_proto::guest::FamilyConnectivity> for FamilyRecord {
    fn from(f: clankerd_proto::guest::FamilyConnectivity) -> Self {
        Self {
            local_available: f.local_available,
            default_route: f.default_route,
            external: match f.external {
                clankerd_proto::guest::ExternalConnectivity::Unavailable => "unavailable",
                clankerd_proto::guest::ExternalConnectivity::Unverified => "unverified",
                clankerd_proto::guest::ExternalConnectivity::Available => "available",
            }
            .into(),
            probe: f.probe.map(|p| ProbeRecord {
                destination: p.destination.to_string(),
                connected: p.connected,
                checked_at: p.checked_at_unix_seconds.saturating_mul(1000),
                diagnostic: p.diagnostic,
            }),
        }
    }
}
#[derive(Serialize, JsonSchema)]
pub struct SubnetRecord {
    pub address: String,
    pub prefix_len: u8,
}
#[derive(Serialize, JsonSchema)]
pub struct NetworkRecord {
    pub id: String,
    pub name: String,
    pub mode: String,
    pub subnet: SubnetRecord,
    pub ipv6_prefix: Option<String>,
    pub created: u64,
}
#[derive(Serialize, JsonSchema)]
pub struct VolumeRecord {
    pub name: String,
    pub path: String,
    pub size: u64,
    pub created: u64,
}
#[derive(Serialize, JsonSchema)]
pub struct ImageRecord {
    pub id: String,
    pub references: Vec<String>,
    pub layers: Vec<LayerRecord>,
    pub config: String,
    pub size: u64,
    pub pulled: u64,
}
#[derive(Serialize, JsonSchema)]
pub struct LayerRecord {
    pub digest: String,
    pub media_type: String,
    pub size: u64,
}
fn timestamp(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}
impl From<MachineInfo> for MachineRecord {
    fn from(i: MachineInfo) -> Self {
        let c = i.config;
        let h = i.host_config;
        let s = i.state;
        Self {
            id: i.id,
            name: i.name,
            created: timestamp(i.created),
            image_id: i.image_id,
            config: ConfigRecord {
                image: c.image,
                entrypoint: c.entrypoint,
                cmd: c.cmd,
                env: c.env,
                user: c.user,
                working_dir: c.working_dir,
                tty: c.tty,
                open_stdin: c.open_stdin,
                stop_signal: c.stop_signal,
            },
            host_config: HostRecord {
                virtual_network: h.virtual_network,
                network_mode: match h.network_mode {
                    libclankerd::NetworkMode::Shared => "shared",
                    libclankerd::NetworkMode::HostOnly => "host-only",
                    libclankerd::NetworkMode::Bridged => "bridged",
                }
                .into(),
                bridge_interface: h.bridge_interface,
                shared_network: SubnetRecord {
                    address: h.shared_network.address.to_string(),
                    prefix_len: h.shared_network.prefix_len,
                },
                shared_ipv6_prefix: h.shared_ipv6_prefix.map(|p| p.to_string()),
                cpus: h.cpus,
                memory: h.memory,
                mounts: h
                    .mounts
                    .into_iter()
                    .map(|m| match m {
                        Mount::Bind {
                            source,
                            target,
                            read_only,
                            semantics,
                        } => MountRecord::Bind {
                            source: source.display().to_string(),
                            target: target.display().to_string(),
                            read_only,
                            semantics,
                        },
                        Mount::Volume { name, target, .. } => MountRecord::Volume {
                            name,
                            target: target.display().to_string(),
                        },
                    })
                    .collect(),
                socket_bindings: h
                    .socket_bindings
                    .into_iter()
                    .map(|s| SocketRecord {
                        host_path: s.host_path.display().to_string(),
                        guest_path: s.guest_path.display().to_string(),
                    })
                    .collect(),
                restart_policy: h.restart_policy.to_string(),
                pull_policy: h.pull_policy.to_string(),
            },
            state: StateRecord {
                status: s.status.to_string(),
                exit_code: s.exit_code,
                oom_killed: s.oom_killed,
                error: s.error,
                pid: s.pid,
                started_at: s.started_at.map(timestamp),
                finished_at: s.finished_at.map(timestamp),
                restart_count: s.restart_count,
            },
            network: i.network.map(|n| NetworkStatusRecord {
                addresses: n.addresses.into_iter().map(|a| a.to_string()).collect(),
                address_details: n
                    .address_details
                    .into_iter()
                    .map(|a| AddressRecord {
                        address: a.address.to_string(),
                        prefix_len: a.prefix_len,
                        interface: a.interface,
                        scope_id: a.scope_id,
                        tentative: a.tentative,
                        dad_failed: a.dad_failed,
                        deprecated: a.deprecated,
                    })
                    .collect(),
                routes: n
                    .routes
                    .into_iter()
                    .map(|r| RouteRecord {
                        destination: r.destination.to_string(),
                        prefix_len: r.prefix_len,
                        gateway: r.gateway.map(|g| g.to_string()),
                        interface: r.interface,
                        metric: r.metric,
                    })
                    .collect(),
                nameservers: n.nameservers,
                search: n.search,
                ipv4: n.ipv4.into(),
                ipv6: n.ipv6.into(),
                ready: n.ready,
                diagnostic: n.diagnostic,
            }),
        }
    }
}
impl From<VirtualNetwork> for NetworkRecord {
    fn from(n: VirtualNetwork) -> Self {
        Self {
            id: n.id,
            name: n.name,
            mode: match n.mode {
                libclankerd::NetworkKind::Shared => "shared",
                libclankerd::NetworkKind::HostOnly => "host-only",
            }
            .into(),
            subnet: SubnetRecord {
                address: n.subnet.address.to_string(),
                prefix_len: n.subnet.prefix_len,
            },
            ipv6_prefix: n.ipv6_prefix.map(|p| p.to_string()),
            created: timestamp(n.created),
        }
    }
}
impl From<VolumeInfo> for VolumeRecord {
    fn from(v: VolumeInfo) -> Self {
        Self {
            name: v.name,
            path: v.path.display().to_string(),
            size: v.size,
            created: timestamp(v.created),
        }
    }
}
impl From<ImageInfo> for ImageRecord {
    fn from(i: ImageInfo) -> Self {
        let size = i.size();
        Self {
            id: i.id,
            references: i.references,
            config: i.config,
            size,
            pulled: timestamp(i.pulled),
            layers: i
                .layers
                .into_iter()
                .map(|l| LayerRecord {
                    digest: l.digest,
                    media_type: l.media_type,
                    size: l.size,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Copy, Default, PartialEq)]
pub enum Output {
    #[default]
    Human,
    Json,
}
impl Output {
    pub fn json(&self, value: &impl Serialize) {
        println!(
            "{}",
            serde_json::to_string_pretty(value).expect("serialize CLI result")
        );
    }
    pub fn diagnostic(&self, target: Option<&str>, error: &Error) {
        let record = diagnostic(target, error);
        if *self == Self::Json {
            eprintln!("{}", serde_json::to_string(&record).unwrap());
        } else {
            eprintln!(
                "Error: {}{}",
                target.map(|t| format!("{t}: ")).unwrap_or_default(),
                error
            );
        }
    }
    pub fn batch(&self, names: &[String], mut f: impl FnMut(&str) -> Result<String, Error>) -> u8 {
        let mut results = Vec::new();
        let mut code = 0;
        for target in names {
            let (id, error) = match f(target) {
                Ok(id) => {
                    if *self == Self::Human {
                        println!("{id}");
                    }
                    (Some(id), None)
                }
                Err(e) => {
                    self.diagnostic(Some(target), &e);
                    code = 1;
                    (None, Some(diagnostic(Some(target), &e)))
                }
            };
            results.push(Operation {
                target: target.clone(),
                success: error.is_none(),
                id,
                error,
            });
        }
        if *self == Self::Json {
            self.json(&results);
        }
        code
    }
    pub fn inspect<T: Serialize>(
        &self,
        names: &[String],
        mut f: impl FnMut(&str) -> Result<T, Error>,
    ) -> u8 {
        let mut records = Vec::new();
        let mut code = 0;
        for name in names {
            match f(name) {
                Ok(r) => records.push(r),
                Err(e) => {
                    self.diagnostic(Some(name), &e);
                    code = 1;
                }
            }
        }
        self.json(&records);
        code
    }
    pub fn table<T: Serialize + TableRecord>(&self, records: &[T]) {
        if *self == Self::Json {
            self.json(&records);
            return;
        }
        let rows: Vec<Vec<String>> = records.iter().map(TableRecord::cells).collect();
        let widths: Vec<usize> = T::COLUMNS
            .iter()
            .enumerate()
            .map(|(i, c)| {
                rows.iter()
                    .map(|r| r[i].len())
                    .max()
                    .unwrap_or(0)
                    .max(c.len())
                    + 2
            })
            .collect();
        for row in std::iter::once(T::COLUMNS.iter().map(|s| s.to_string()).collect()).chain(rows) {
            for (i, cell) in row.iter().enumerate() {
                if i + 1 == row.len() {
                    print!("{cell}");
                } else {
                    print!("{cell:<width$}", width = widths[i]);
                }
            }
            println!();
        }
    }
}
fn diagnostic(target: Option<&str>, e: &Error) -> Diagnostic {
    Diagnostic {
        origin: "operation",
        target: target.map(str::to_owned),
        kind: format!("{:?}", e.kind()),
        message: e.to_string(),
    }
}
pub trait TableRecord {
    const COLUMNS: &'static [&'static str];
    fn cells(&self) -> Vec<String>;
}
impl TableRecord for MachineRecord {
    const COLUMNS: &'static [&'static str] = &["MACHINE ID", "IMAGE", "STATUS", "NAMES"];
    fn cells(&self) -> Vec<String> {
        vec![
            self.id.chars().take(12).collect(),
            self.config.image.clone(),
            match self.state.status.as_str() {
                "running" => "Up".into(),
                "exited" => format!("Exited ({})", self.state.exit_code),
                other => {
                    let mut chars = other.chars();
                    chars
                        .next()
                        .map(|c| c.to_uppercase().collect::<String>() + chars.as_str())
                        .unwrap_or_default()
                }
            },
            self.name.clone(),
        ]
    }
}
impl TableRecord for NetworkRecord {
    const COLUMNS: &'static [&'static str] = &["NETWORK NAME", "MODE", "SUBNET"];
    fn cells(&self) -> Vec<String> {
        vec![
            self.name.clone(),
            self.mode.clone(),
            format!("{}/{}", self.subnet.address, self.subnet.prefix_len),
        ]
    }
}
impl TableRecord for VolumeRecord {
    const COLUMNS: &'static [&'static str] = &["VOLUME NAME", "SIZE"];
    fn cells(&self) -> Vec<String> {
        vec![self.name.clone(), crate::contract::binary_size(self.size)]
    }
}
impl TableRecord for ImageRecord {
    const COLUMNS: &'static [&'static str] = &["IMAGE ID", "REFERENCES", "SIZE"];
    fn cells(&self) -> Vec<String> {
        vec![
            self.id.clone(),
            self.references.join(", "),
            crate::contract::binary_size(self.size),
        ]
    }
}
