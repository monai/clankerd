//! Authoritative CLI requests and command metadata. Parsing and help share these definitions.
use crate::parse;
use clap::{Args, Parser, Subcommand};
use libclankerd::{Mount, PullPolicy, RestartPolicy, SharedNetwork, SocketBinding};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "vmctl", version, about = "Run Linux machines from OCI images")]
pub struct Cli {
    /// Machine configuration and state directory.
    #[arg(long, env = "CLANKERD_STATE_DIR", value_name = "DIR")]
    pub state_dir: Option<PathBuf>,
    /// Runtime socket directory.
    #[arg(long, env = "CLANKERD_RUNTIME_DIR", value_name = "DIR")]
    pub runtime_dir: Option<PathBuf>,
    /// Development: run clankerd-guestd from this path as a local process
    /// instead of booting a VM.
    #[arg(long, env = "CLANKERD_DEV_GUESTD", hide = true)]
    pub dev_guestd: Option<PathBuf>,
    /// Image cache directory.
    #[arg(long, env = "CLANKERD_CACHE_DIR", value_name = "DIR")]
    pub cache_dir: Option<PathBuf>,
    /// Use plain HTTP for this registry. Repeatable.
    #[arg(
        long = "insecure-registry",
        env = "CLANKERD_INSECURE_REGISTRIES",
        value_name = "HOST:PORT",
        value_delimiter = ','
    )]
    pub insecure_registries: Vec<String>,
    /// Development: directory with the static e2fsprogs (mke2fs); defaults to
    /// the directory of --dev-guestd.
    #[arg(long, env = "CLANKERD_BOOT_DIR", hide = true)]
    pub boot_dir: Option<PathBuf>,
    /// Boot the machine's `boot/` directory as the guest root instead of an
    /// image's root disk (the first-boot check of docs/libkrun-first-boot.md).
    #[arg(long, env = "CLANKERD_BOOT_DIR_ROOT", hide = true)]
    pub boot_dir_root: bool,
    /// Machine launcher executable.
    #[arg(long, env = "CLANKERD_VMSPAWN", value_name = "PATH")]
    pub vmspawn: Option<PathBuf>,
    /// Guest init executable.
    #[arg(long, env = "CLANKERD_GUESTD", value_name = "PATH")]
    pub guestd: Option<PathBuf>,
    /// Clankerd native networking executable.
    #[arg(long, env = "CLANKERD_NETWORK", value_name = "PATH")]
    pub vmnet_helper: Option<PathBuf>,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Manage machines.
    #[command(subcommand)]
    Machine(Box<MachineCommand>),
    /// Manage OCI images.
    #[command(subcommand)]
    Image(ImageCommand),
    /// Manage virtual networks and their isolation boundaries.
    #[command(subcommand)]
    Network(NetworkCommand),
    /// Manage named volumes.
    #[command(subcommand)]
    Volume(VolumeCommand),
}

#[derive(Subcommand)]
pub enum ImageCommand {
    /// Pull a linux/arm64 OCI image into the local cache.
    Pull { image: String },
    /// List cached images.
    Ls,
}

#[derive(Subcommand)]
pub enum MachineCommand {
    /// Create and boot a machine; return the image command status, leaving guestd running.
    Run(RunArgs),
    /// Create a machine without starting it.
    Create(CreateArgs),
    /// Start created or exited machines.
    Start {
        #[arg(required = true)]
        machines: Vec<String>,
    },
    /// Patch saved host configuration of stopped machines.
    Update(UpdateArgs),
    /// Replace a stopped machine's image root, retaining volumes and overrides.
    Reimage { machine: String, image: String },
    /// Stop machines gracefully: the workload gets its stop signal (SIGTERM unless the
    /// image's StopSignal or --stop-signal says otherwise), the machine syncs,
    /// unmounts and powers off; it is killed after the timeout.
    Stop {
        /// Seconds to wait before killing.
        #[arg(short = 't', long = "time", default_value_t = 10)]
        time: u64,
        #[arg(required = true)]
        machines: Vec<String>,
    },
    /// Forcibly terminate machines.
    Kill {
        #[arg(required = true)]
        machines: Vec<String>,
    },
    /// List machines.
    #[command(name = "ls")]
    Ps {
        /// Show all machines, not only running ones.
        #[arg(short, long)]
        all: bool,
    },
    /// Show machine configuration and state as JSON.
    Inspect {
        #[arg(required = true)]
        machines: Vec<String>,
    },
    /// Print the console output (kernel, guestd and workload) of a machine.
    Logs { machine: String },
    /// Run a command in a running machine.
    Exec(ExecArgs),
    /// Remove machines, retaining named volumes.
    Rm {
        /// Remove running machines too.
        #[arg(short, long)]
        force: bool,
        #[arg(required = true)]
        machines: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum NetworkCommand {
    /// List virtual networks.
    Ls,
    /// Create a virtual network owning its mode and subnet.
    Create {
        name: String,
        /// Virtual network mode: shared or host-only.
        #[arg(long, default_value = "shared", value_parser = mode_parser())]
        mode: libclankerd::NetworkKind,
        /// Aligned RFC1918 IPv4 CIDR; omitted: allocate an unused /24.
        #[arg(long)]
        subnet: Option<SharedNetwork>,
    },
    /// Inspect named virtual networks as JSON.
    Inspect {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Remove unused virtual networks.
    Rm {
        #[arg(required = true)]
        names: Vec<String>,
    },
}

#[derive(Subcommand)]
pub enum VolumeCommand {
    /// List volumes.
    Ls,
    /// Create a sparse volume ahead of time (machines also create theirs).
    Create {
        /// Bytes or integer K/M/G/T, optionally B/iB (powers of 1024); default 16G.
        #[arg(long, value_parser = parse::size)]
        size: Option<u64>,
        name: String,
    },
    /// Show volumes as JSON.
    Inspect {
        #[arg(required = true)]
        names: Vec<String>,
    },
    /// Delete volumes and their data (not while a machine mounts them).
    Rm {
        #[arg(required = true)]
        names: Vec<String>,
    },
}

#[derive(Args)]
pub struct RunArgs {
    /// Print the machine identifier after startup, without waiting for the image command.
    #[arg(short = 'd', long)]
    pub detach: bool,
    #[command(flatten)]
    pub create: CreateArgs,
}

#[derive(Args, Clone)]
pub struct CreateArgs {
    /// Machine name; omitted: library-generated name.
    #[arg(long)]
    pub name: Option<String>,
    /// Environment variable, KEY=value.
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,
    /// Guest user (image default when omitted).
    #[arg(short = 'u', long)]
    pub user: Option<String>,
    /// Guest working directory (image default when omitted).
    #[arg(short = 'w', long)]
    pub workdir: Option<String>,
    /// Replace the image entrypoint executable.
    #[arg(long)]
    pub entrypoint: Option<String>,
    /// Keep stdin open (attach it with `vmctl run -i`).
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// Allocate a pseudo-terminal for the main process.
    #[arg(short = 't', long)]
    pub tty: bool,
    /// Signal `vmctl stop` sends the main process (default: the image's StopSignal, else SIGTERM).
    #[arg(long = "stop-signal", value_name = "SIGNAL")]
    pub stop_signal: Option<String>,
    #[command(flatten)]
    pub host: HostArgs,
    /// Typed mount: type=bind|volume,source=SOURCE,target=/GUEST[,readonly][,semantics=simplified|complete]. Bind semantics default to simplified (caller ownership; chmod changes host permissions); complete preserves guest metadata.
    #[arg(long, value_parser = parse::typed_mount)]
    pub mount: Vec<Mount>,
    /// Mount SOURCE:/GUEST[:ro|rw]; bind semantics default to simplified. Use --mount for complete semantics; repeat to accumulate mounts.
    #[arg(short = 'v', long, value_parser = parse::parse_mount)]
    pub volume: Vec<Mount>,
    /// Persist a host Unix socket binding: host=/HOST,guest=/GUEST.
    #[arg(long, value_parser = parse::socket)]
    pub socket: Vec<SocketBinding>,
    pub image: String,
    /// Command and arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cmd: Vec<String>,
}

#[derive(Args, Default, Clone)]
pub struct HostArgs {
    /// Virtual network name. Omitted on create: default shared network.
    #[arg(long, conflicts_with = "bridge_interface")]
    pub network: Option<String>,
    /// Physical LAN interface; selects bridged attachment.
    #[arg(long)]
    pub bridge_interface: Option<String>,
    /// Virtual CPU count (positive integer); omitted: library default.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..))]
    pub cpus: Option<u32>,
    /// Bytes or integer K/M/G/T, optionally B/iB (powers of 1024).
    #[arg(short = 'm', long, value_parser = parse::size)]
    pub memory: Option<u64>,
    /// Machine restart policy: no, always, on-failure[:N], unless-stopped. Create default: no.
    #[arg(long)]
    pub restart: Option<RestartPolicy>,
    /// Image pull policy: missing, always, never. Create default: missing.
    #[arg(long, value_parser = pull_parser())]
    pub pull: Option<PullPolicy>,
}

#[derive(Args)]
pub struct UpdateArgs {
    #[command(flatten)]
    pub host: HostArgs,
    /// Add a typed mount: type=bind|volume,source=SOURCE,target=/GUEST[,readonly][,semantics=simplified|complete]. Bind semantics default to simplified; semantics and readonly are bind-only.
    #[arg(long, value_parser = parse::typed_mount)]
    pub mount_add: Vec<Mount>,
    /// Ensure the mount at this absolute guest target is detached; absent targets succeed; data is retained.
    #[arg(long, value_parser = parse::guest_path)]
    pub mount_remove: Vec<PathBuf>,
    /// Add a persisted socket binding: host=/HOST,guest=/GUEST.
    #[arg(long, value_parser = parse::socket)]
    pub socket_add: Vec<SocketBinding>,
    /// Ensure the socket binding at this absolute guest endpoint is absent; absent targets succeed.
    #[arg(long, value_parser = parse::guest_path)]
    pub socket_remove: Vec<PathBuf>,
    #[arg(required = true)]
    pub machines: Vec<String>,
}

impl HostArgs {
    pub fn apply(
        &self,
        engine: &libclankerd::Engine,
        host: &mut libclankerd::HostConfig,
    ) -> Result<(), libclankerd::Error> {
        if let Some(name) = &self.network {
            engine.inspect_network(name)?.configure_member(host);
        }
        if let Some(interface) = &self.bridge_interface {
            host.virtual_network = None;
            host.network_mode = libclankerd::NetworkMode::Bridged;
            host.bridge_interface = Some(interface.clone());
            host.shared_network = SharedNetwork::default();
            host.shared_ipv6_prefix = None;
            host.shared_peer_participation = true;
        }
        if let Some(value) = self.cpus {
            host.cpus = Some(value);
        }
        if let Some(value) = self.memory {
            host.memory = Some(value);
        }
        if let Some(value) = self.restart {
            host.restart_policy = value;
        }
        if let Some(value) = self.pull {
            host.pull_policy = value;
        }
        Ok(())
    }
    fn is_empty(&self) -> bool {
        self.network.is_none()
            && self.bridge_interface.is_none()
            && self.cpus.is_none()
            && self.memory.is_none()
            && self.restart.is_none()
            && self.pull.is_none()
    }
}

impl UpdateArgs {
    pub fn apply(
        &self,
        engine: &libclankerd::Engine,
        host: &mut libclankerd::HostConfig,
    ) -> Result<(), libclankerd::Error> {
        self.host.apply(engine, host)?;
        for target in &self.mount_remove {
            host.mounts.retain(|m| m.target() != target);
        }
        host.mounts.extend(self.mount_add.clone());
        for target in &self.socket_remove {
            host.socket_bindings.retain(|s| &s.guest_path != target);
        }
        host.socket_bindings.extend(self.socket_add.clone());
        Ok(())
    }
}

pub fn invocation() -> (Cli, results::Output) {
    use clap::FromArgMatches;
    let mut args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    // Shortcuts expand before clap normalizes the typed request.
    let root = definition();
    let mut i = 1;
    while i < args.len() {
        let token = args[i].to_string_lossy();
        if !token.starts_with('-') {
            break;
        }
        let name = token.split('=').next().unwrap();
        let option = root
            .get_arguments()
            .find(|a| a.get_long().is_some_and(|long| name == format!("--{long}")));
        i += if option.is_some_and(|a| a.get_action().takes_values()) && !token.contains('=') {
            2
        } else {
            1
        };
    }
    if i < args.len() {
        let name = args[i].to_string_lossy();
        let canonical = root
            .get_subcommands()
            .filter(|c| matches!(c.get_name(), "machine" | "image"))
            .find_map(|resource| {
                resource
                    .get_subcommands()
                    .find(|a| shortcut(resource.get_name(), a.get_name()) == name)
                    .map(|a| (resource.get_name().to_owned(), a.get_name().to_owned()))
            });
        if let Some((resource, action)) = canonical {
            args.splice(i..=i, [resource.into(), action.into()]);
        }
    }
    let mut definition = definition();
    definition.build();
    if args.len() == 2 && args[1] == "--describe-contract" {
        println!(
            "{}",
            serde_json::to_string_pretty(&metadata(&definition)).unwrap()
        );
        std::process::exit(0);
    }
    let mut json = false;
    if let Some(index) =
        validate_order(&definition, &args, &mut json).unwrap_or_else(|e| invocation_error(e, json))
    {
        args.insert(index, "--".into());
    }
    let matches = definition
        .try_get_matches_from(args)
        .unwrap_or_else(|e| invocation_error(e, json));
    let mut leaf = &matches;
    while let Some((_, child)) = leaf.subcommand() {
        leaf = child;
    }
    let output = if leaf
        .try_get_one::<String>("output")
        .ok()
        .flatten()
        .is_some_and(|s| s == "json")
    {
        results::Output::Json
    } else {
        results::Output::Human
    };
    let mut cli = Cli::from_arg_matches(&matches).unwrap_or_else(|e| invocation_error(e, json));
    if let Command::Machine(command) = &mut cli.command
        && let MachineCommand::Create(create) | MachineCommand::Run(RunArgs { create, .. }) =
            command.as_mut()
    {
        let mut mounts: Vec<_> = ["mount", "volume"]
            .into_iter()
            .flat_map(|id| {
                leaf.indices_of(id)
                    .into_iter()
                    .flatten()
                    .zip(leaf.get_many::<Mount>(id).into_iter().flatten())
                    .map(|(index, mount)| (index, mount.clone()))
            })
            .collect();
        mounts.sort_by_key(|(index, _)| *index);
        create.mount = mounts.into_iter().map(|(_, mount)| mount).collect();
        create.volume.clear();
    }
    if let Command::Machine(command) = &cli.command
        && let MachineCommand::Update(a) = command.as_ref()
        && a.host.is_empty()
        && a.mount_add.is_empty()
        && a.mount_remove.is_empty()
        && a.socket_add.is_empty()
        && a.socket_remove.is_empty()
    {
        invocation_error(
            clap::Error::raw(
                clap::error::ErrorKind::MissingRequiredArgument,
                "update requires at least one configuration change",
            ),
            json,
        );
    }
    if output == results::Output::Json
        && matches!(&cli.command, Command::Machine(command) if matches!(command.as_ref(), MachineCommand::Run(a) if !a.detach))
    {
        invocation_error(
            clap::Error::raw(
                clap::error::ErrorKind::ArgumentConflict,
                "--output json requires detached run (--detach)",
            ),
            json,
        );
    }
    (cli, output)
}

pub mod results;

fn definition() -> clap::Command {
    use clap::CommandFactory;
    let mut root = Cli::command();
    root = root.arg(
        clap::Arg::new("describe-contract")
            .long("describe-contract")
            .action(clap::ArgAction::SetTrue)
            .help("Print command metadata and result schemas as JSON")
            .exclusive(true),
    );
    root = decorate(root, 0);
    let mut shortcuts = Vec::new();
    for resource in root.get_subcommands() {
        if matches!(resource.get_name(), "machine" | "image") {
            for action in resource.get_subcommands() {
                let name = shortcut(resource.get_name(), action.get_name());
                shortcuts.push(action.clone().name(name).about(format!(
                    "Shortcut for {} {}",
                    resource.get_name(),
                    action.get_name()
                )));
            }
        }
    }
    for action in shortcuts {
        root = root.subcommand(action);
    }
    root.after_help("Global options precede the resource or shortcut. Action options precede operands.\nCreate/run: IMAGE [COMMAND [ARG...]]; exec: MACHINE COMMAND [ARG...].\nAfter IMAGE or exec MACHINE all tokens belong to the guest, with no shell interpretation.\nUse -- before operands to end option parsing; a -- in the guest tail is passed literally.\nExecutable paths are resolved automatically when omitted.\nCLI values override environment values, which override documented defaults.")
}
fn shortcut(resource: &str, action: &str) -> String {
    match (resource, action) {
        ("machine", "ls") => "ps".into(),
        ("image", "ls") => "images".into(),
        _ => action.into(),
    }
}
fn decorate(mut command: clap::Command, depth: usize) -> clap::Command {
    let name = command.get_name().to_owned();
    if depth == 2
        && let Some(kind) = result_kind(&name)
    {
        let policy = kind.policy(None);
        if !policy.formats.is_empty() {
            command = command.arg(
                clap::Arg::new("output")
                    .long("output")
                    .value_name("FORMAT")
                    .value_parser(policy.formats.to_vec())
                    .help(format!("Result representation (default: {})", policy.human)),
            );
        }
    }
    command = command.next_display_order(Some(0));
    let ids: Vec<_> = command
        .get_arguments()
        .map(|a| a.get_id().clone())
        .collect();
    for (order, id) in ids.into_iter().enumerate() {
        command = command.mut_arg(id, |arg| {
            let arg = if arg.get_help().is_none() {
                let help = match arg.get_id().as_str() {
                    "image" => "OCI image reference.",
                    "machine" | "machines" => "Existing machine name or identifier.",
                    "name" | "names" => {
                        "Resource name (name lookup only for networks and volumes)."
                    }
                    _ => "Command operand.",
                };
                arg.help(help)
            } else {
                arg
            };
            let arg = if depth == 0 {
                arg.hide_env_values(true)
            } else {
                arg
            };
            let group = if depth == 0 {
                match arg.get_id().as_str() {
                    "vmspawn" | "guestd" | "vmnet_helper" => "Executable overrides (global)",
                    _ => "Global options",
                }
            } else {
                match arg.get_id().as_str() {
                    "cpus" | "memory" | "restart" | "pull" => "Host configuration",
                    "network" | "bridge_interface" | "mode" | "subnet" => "Networking",
                    "mount" | "volume" | "socket" | "mount_add" | "mount_remove" | "socket_add"
                    | "socket_remove" => "Storage and sockets",
                    "output" => "Output",
                    _ => "Action options",
                }
            };
            if arg.is_positional() {
                arg.display_order(order)
            } else {
                arg.help_heading(group).display_order(order)
            }
        });
    }
    let children: Vec<_> = command
        .get_subcommands()
        .map(|c| c.get_name().to_owned())
        .collect();
    for (order, name) in children.into_iter().enumerate() {
        command = command.mut_subcommand(name, |c| decorate(c, depth + 1).display_order(order));
    }
    command
}

/// Enforce scopes before clap parses values. Insert `--` at the guest tail so even
/// management-looking tokens immediately after the leading operand stay literal.
fn validate_order(
    command: &clap::Command,
    args: &[std::ffi::OsString],
    json: &mut bool,
) -> Result<Option<usize>, clap::Error> {
    let mut current = command;
    let mut operands = false;
    let mut ended = false;
    let mut index = 1;
    while index < args.len() {
        let token = args[index].to_string_lossy();
        if token == "--" {
            ended = true;
            index += 1;
            continue;
        }
        if !ended && token.starts_with('-') && token != "-" {
            if operands {
                return Err(clap::Error::raw(
                    clap::error::ErrorKind::UnknownArgument,
                    "action options must precede operands",
                ));
            }
            if token.starts_with("--") {
                if token == "--output=json"
                    || (token == "--output" && args.get(index + 1).is_some_and(|v| v == "json"))
                {
                    *json = true;
                }
                let name = token.trim_start_matches('-').split('=').next().unwrap();
                if let Some(a) = current.get_arguments().find(|a| a.get_long() == Some(name))
                    && a.get_action().takes_values()
                    && !token.contains('=')
                {
                    index += 1;
                }
            } else {
                for (offset, short) in token[1..].char_indices() {
                    if let Some(a) = current
                        .get_arguments()
                        .find(|a| a.get_short() == Some(short))
                        && a.get_action().takes_values()
                    {
                        if offset + short.len_utf8() == token.len() - 1 {
                            index += 1;
                        }
                        break;
                    }
                }
            }
        } else if !ended
            && !operands
            && let Some(child) = current.find_subcommand(token.as_ref())
        {
            current = child;
        } else {
            operands = true;
            if matches!(current.get_name(), "run" | "create" | "exec")
                && current.get_arguments().any(|a| a.get_id() == "cmd")
            {
                return Ok((!ended).then_some(index));
            }
        }
        index += 1;
    }
    Ok(None)
}

fn metadata(command: &clap::Command) -> serde_json::Value {
    use serde_json::json;
    fn node(root: &clap::Command, c: &clap::Command, resource: Option<&str>) -> serde_json::Value {
        let canonical = resource.map(|r| (r, c.get_name())).or_else(|| {
            root.get_subcommands()
                .filter(|r| matches!(r.get_name(), "machine" | "image"))
                .find_map(|r| {
                    r.get_subcommands()
                        .find(|a| shortcut(r.get_name(), a.get_name()) == c.get_name())
                        .map(|a| (r.get_name(), a.get_name()))
                })
        });
        let (resource, action) = canonical
            .map(|(r, a)| (Some(r), a))
            .unwrap_or((resource, c.get_name()));
        json!({ "name": c.get_name(), "summary": c.get_about().map(|s| s.to_string()),
            "arguments": c.get_arguments().filter(|a| a.get_id() != "help").map(|a| json!({
                "name": a.get_id().as_str(), "long": a.get_long(), "short": a.get_short(),
                "value_names": a.get_value_names().map(|v| v.iter().map(|s| s.as_str()).collect::<Vec<_>>()),
                "summary": a.get_help().map(|s| s.to_string()), "group": a.get_help_heading(),
                "order": a.get_display_order(), "operand": a.is_positional(), "required": a.is_required_set(),
                "cardinality": a.get_num_args().map(|n| json!({"min": n.min_values(), "max": if n.max_values() == usize::MAX { None } else { Some(n.max_values()) }})),
                "repetition": if matches!(a.get_action(), clap::ArgAction::Append) { "accumulate" } else { "reject" },
                "input": input_contract(a),
                "values": a.get_value_parser().possible_values().map(|v| v.map(|p| p.get_name().to_owned()).collect::<Vec<_>>()),
                "effective_default": effective_default(a, resource, action),
                "hidden": a.is_hide_set(),
                "defaults": a.get_default_values().iter().map(|v| v.to_string_lossy()).collect::<Vec<_>>(),
                "environment": a.get_env().map(|s| s.to_string_lossy()),
                "conflicts": c.get_arg_conflicts_with(a).iter().map(|a| a.get_id().as_str()).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "result": result_contract(resource, action),
            "commands": c.get_subcommands().filter(|c| c.get_name() != "help").map(|child| node(root, child, if matches!(c.get_name(), "machine" | "image" | "network" | "volume") { Some(c.get_name()) } else { resource })).collect::<Vec<_>>() })
    }
    json!({"version": 1, "command": node(command, command, None),
        "aliases": command.get_subcommands().filter(|c| matches!(c.get_name(), "machine" | "image")).flat_map(|r| r.get_subcommands().filter(|a| a.get_name() != "help").map(move |a| json!({"shortcut":shortcut(r.get_name(),a.get_name()),"canonical":[r.get_name(),a.get_name()]}))).collect::<Vec<_>>(),
        "grammar": {"global_options": "before resource or shortcut", "action_options": "before operands", "guest_tail": "after create/run IMAGE or exec MACHINE; literal argv, including --", "end_of_options": "-- before operands ends management option parsing"},
        "precedence": ["CLI", "environment", "default"], "timestamps": "UTC milliseconds since Unix epoch; absent timestamps are null",
        "exit_codes": {"success":0,"operation_or_partial_failure":1,"invalid_invocation":2,"process":"foreground run/exec return process exit status when available"},
        "empty_operation_results":"[] on setup failure; image pull uses null when no image result exists",
        "identifiers":{"machine_create_run":"full machine identifier", "machine_existing_mutations":"supplied name or identifier", "image":"sha256 digest", "network_volume":"name"},
        "collections": {"env":"key-based library override", "mounts":"unique guest targets and named volume sources; multiple named volumes supported", "sockets":"unique guest endpoints", "update":"remove then add; omitted fields preserved; at least one change required", "removal":{"absent_target":"success", "duplicates":"idempotent", "eligibility":"existing stopped machine"}},
        "eligibility": {"update":"stopped", "reimage":"stopped", "exec":"running"},
        "schemas": {"machine": schemars::schema_for!(results::MachineRecord), "image": schemars::schema_for!(results::ImageRecord), "network": schemars::schema_for!(results::NetworkRecord), "volume": schemars::schema_for!(results::VolumeRecord), "operation": schemars::schema_for!(results::Operation), "diagnostic": schemars::schema_for!(results::Diagnostic)}})
}

fn binary_size(bytes: u64) -> String {
    for (shift, unit) in [(40, "T"), (30, "G"), (20, "M"), (10, "K")] {
        if bytes >= 1 << shift {
            return format!("{:.1}{unit}", bytes as f64 / (1u64 << shift) as f64);
        }
    }
    format!("{bytes}B")
}

#[derive(Args)]
pub struct ExecArgs {
    /// Keep stdin open and forward it.
    #[arg(short = 'i', long)]
    pub interactive: bool,
    /// Allocate a pseudo-terminal.
    #[arg(short = 't', long)]
    pub tty: bool,
    /// Guest user (image default when omitted).
    #[arg(short = 'u', long)]
    pub user: Option<String>,
    /// Guest working directory (image default when omitted).
    #[arg(short = 'w', long)]
    pub workdir: Option<String>,
    /// Environment variable, KEY=value.
    #[arg(short = 'e', long = "env")]
    pub env: Vec<String>,
    pub machine: String,
    /// Command and arguments.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    pub cmd: Vec<String>,
}

fn invocation_error(error: clap::Error, json: bool) -> ! {
    if !json || !error.use_stderr() {
        error.exit();
    }
    eprintln!(
        "{}",
        serde_json::to_string(&results::Diagnostic {
            origin: "invocation",
            target: None,
            kind: "invalid_invocation".into(),
            message: error.to_string()
        })
        .unwrap()
    );
    std::process::exit(2)
}
fn pull_parser() -> impl clap::builder::TypedValueParser<Value = PullPolicy> {
    use clap::builder::TypedValueParser;
    clap::builder::PossibleValuesParser::new(["missing", "always", "never"])
        .map(|s| s.parse().expect("declared pull policy"))
}
fn mode_parser() -> impl clap::builder::TypedValueParser<Value = libclankerd::NetworkKind> {
    use clap::builder::TypedValueParser;
    clap::builder::PossibleValuesParser::new(["shared", "host-only"])
        .map(|s| s.parse().expect("declared network kind"))
}

#[derive(serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum ResultKind {
    List,
    Inspect,
    Mutation,
    Image,
    Stream,
    Run,
}
fn result_kind(action: &str) -> Option<ResultKind> {
    Some(match action {
        "ls" | "ps" | "images" => ResultKind::List,
        "inspect" => ResultKind::Inspect,
        "pull" => ResultKind::Image,
        "logs" | "exec" => ResultKind::Stream,
        "run" => ResultKind::Run,
        "create" | "start" | "stop" | "kill" | "update" | "reimage" | "rm" => ResultKind::Mutation,
        _ => return None,
    })
}
fn input_contract(arg: &clap::Arg) -> serde_json::Value {
    use serde_json::json;
    use std::any::TypeId;
    let t = arg.get_value_parser().type_id();
    if t == TypeId::of::<bool>() {
        return json!({"type":"boolean"});
    }
    if t == TypeId::of::<u32>() {
        return json!({"type":"integer", "minimum":1});
    }
    if t == TypeId::of::<u64>() {
        return if matches!(arg.get_id().as_str(), "memory" | "size") {
            json!({"type":"size", "grammar":"integer with optional K/M/G/T suffix, optionally B/iB; powers of 1024"})
        } else {
            json!({"type":"integer", "minimum":0})
        };
    }
    if t == TypeId::of::<Mount>() {
        return json!({"type":"mount", "syntax": if arg.get_id() == "volume" {"SOURCE:/GUEST[:ro|rw]"} else {"type=bind|volume,source=SOURCE,target=/GUEST[,readonly[=true|false]][,semantics=simplified|complete]"},"target":"absolute non-root guest path without dot components", "bind_source":"host path; ~/ expands HOME; relative paths resolve against current directory", "readonly":"bind only; enforced by host filesystem server and guest mount", "semantics":{"scope":"bind only", "values":["simplified","complete"], "default":"simplified", "simplified":"requesting guest ownership; chown does not persist ownership; chmod changes host permissions", "complete":"guest ownership and permissions stored in host extended attributes", "shorthand":"simplified; use --mount to select complete"}, "shorthand_bind_prefixes":["/","./","../","~/"]});
    }
    if t == TypeId::of::<SocketBinding>() {
        return json!({"type":"socket_binding", "syntax":"host=/HOST,guest=/GUEST", "endpoints":"absolute non-root paths"});
    }
    if t == TypeId::of::<RestartPolicy>() {
        return json!({"type":"restart_policy", "values":["no","always","unless-stopped","on-failure[:N]"],"retry_count":"u32"});
    }
    if t == TypeId::of::<PullPolicy>() {
        return json!({"type":"pull_policy"});
    }
    if t == TypeId::of::<SharedNetwork>() {
        return json!({"type":"IPv4 CIDR", "constraints":"aligned RFC1918, prefix /8 through /30"});
    }
    if t == TypeId::of::<libclankerd::NetworkKind>() {
        return json!({"type":"network_kind"});
    }
    if t == TypeId::of::<PathBuf>() {
        return json!({"type":"path"});
    }
    json!({"type":"string", "reference": match arg.get_id().as_str() { "machine" | "machines" => Some("machine name or identifier"), "network" => Some("existing virtual-network name"), _ => None }})
}

fn result_contract(resource: Option<&str>, action: &str) -> serde_json::Value {
    use results::TableRecord;
    use serde_json::json;
    let Some(kind) = result_kind(action) else {
        return serde_json::Value::Null;
    };
    let columns: &[&str] = match resource {
        Some("machine") => results::MachineRecord::COLUMNS,
        Some("image") => results::ImageRecord::COLUMNS,
        Some("network") => results::NetworkRecord::COLUMNS,
        Some("volume") => results::VolumeRecord::COLUMNS,
        _ => &[],
    };
    let policy = kind.policy(resource);
    json!({"kind":kind,"default_stdout":policy.human,"json_shape":policy.shape,"schema":policy.schema,"allowed_output":policy.formats,"table_columns":columns,
        "eligibility":match action { "update" | "reimage" => "stopped", "exec" => "running", _ => "library lifecycle rules" },
        "batch":"every independent supplied target attempted in input order; no rollback"})
}

struct ResultPolicy<'a> {
    human: &'static str,
    shape: &'static str,
    schema: Option<&'a str>,
    formats: &'static [&'static str],
}
impl ResultKind {
    fn policy<'a>(&self, resource: Option<&'a str>) -> ResultPolicy<'a> {
        let (human, shape, schema, formats): (_, _, _, &[&str]) = match self {
            Self::List => ("table", "array", resource, &["human", "json"]),
            Self::Inspect => ("json", "array", resource, &["json"]),
            Self::Mutation => (
                "identifier per success",
                "array",
                Some("operation"),
                &["json"],
            ),
            Self::Image => (
                "image digest",
                "object or null on failure",
                Some("image"),
                &["json"],
            ),
            Self::Stream => ("original bytes", "unavailable", None, &[]),
            Self::Run => (
                "original bytes; detached: identifier",
                "detached only: array",
                Some("operation"),
                &["json"],
            ),
        };
        ResultPolicy {
            human,
            shape,
            schema,
            formats,
        }
    }
}
fn effective_default(arg: &clap::Arg, resource: Option<&str>, action: &str) -> serde_json::Value {
    use serde_json::json;
    let field = arg.get_id().as_str();
    if action == "update"
        && matches!(
            field,
            "cpus"
                | "memory"
                | "pull"
                | "restart"
                | "network"
                | "bridge_interface"
                | "mount_add"
                | "mount_remove"
                | "socket_add"
                | "socket_remove"
        )
    {
        return json!({"source":"preserve_saved_value"});
    }
    if resource == Some("machine") && matches!(action, "create" | "run") {
        let host = libclankerd::HostConfig::default();
        match field {
            "pull" => return json!({"source":"library", "value":host.pull_policy.to_string()}),
            "restart" => {
                return json!({"source":"library", "value":host.restart_policy.to_string()});
            }
            "network" => {
                return json!({"source":"library", "value":format!("default-{}", serde_json::to_value(host.network_mode).unwrap().as_str().unwrap()), "unless_explicit":"bridge_interface"});
            }
            "cpus" | "memory" => return json!({"source":"backend_default"}),
            "cmd" | "entrypoint" | "user" | "workdir" | "env" | "stop_signal" => {
                return json!({"source":"OCI_image_defaults"});
            }
            "mount" | "volume" | "socket" => {
                return json!({"source":"empty_collection", "value":[]});
            }
            _ => {}
        }
    }
    if resource == Some("volume") && action == "create" && field == "size" {
        return json!({"source":"library", "value":libclankerd::DEFAULT_VOLUME_SIZE});
    }
    if resource == Some("network") && action == "create" && field == "subnet" {
        return json!({"source":"library_allocation", "range":"unused /24 in 192.168.64.0/24 through 192.168.254.0/24, reserving 105 and 128"});
    }
    let values: Vec<_> = arg
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy())
        .collect();
    if values.is_empty() {
        json!({"source":"absent"})
    } else {
        json!({"source":"parser", "values":values})
    }
}
