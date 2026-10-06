//! vmctl: Docker-style CLI over libclankerd.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

mod exec;

use clap::{Args, Parser, Subcommand};
use libclankerd::vmm::{LocalProcessVmm, VmspawnVmm};
use libclankerd::{
    Engine, EngineConfig, Error, HostConfig, ImageInfo, LocalGuestdPopulator, MachineConfig,
    MachineInfo, PortBinding, Status, VmspawnPopulator,
};

#[derive(Parser)]
#[command(name = "vmctl", version, about = "Run Linux machines from OCI images")]
struct Cli {
    /// Directory for machine config and state.
    #[arg(long, global = true, env = "CLANKERD_STATE_DIR")]
    state_dir: Option<PathBuf>,
    /// Directory for runtime sockets.
    #[arg(long, global = true, env = "CLANKERD_RUNTIME_DIR")]
    runtime_dir: Option<PathBuf>,
    /// Development: run clankerd-guestd from this path as a local process
    /// instead of booting a VM.
    #[arg(long, global = true, env = "CLANKERD_DEV_GUESTD", hide = true)]
    dev_guestd: Option<PathBuf>,
    /// Directory for the image cache (blobs, base root disks).
    #[arg(long, global = true, env = "CLANKERD_CACHE_DIR")]
    cache_dir: Option<PathBuf>,
    /// Registries (host:port) to reach over plain HTTP.
    #[arg(
        long = "insecure-registry",
        global = true,
        env = "CLANKERD_INSECURE_REGISTRIES",
        value_delimiter = ','
    )]
    insecure_registries: Vec<String>,
    /// Development: directory with the static e2fsprogs (mke2fs); defaults to
    /// the directory of --dev-guestd.
    #[arg(long, global = true, env = "CLANKERD_BOOT_DIR", hide = true)]
    boot_dir: Option<PathBuf>,
    /// Boot the machine's `boot/` directory as the guest root instead of an
    /// image's root disk (the first-boot check of docs/libkrun-first-boot.md).
    #[arg(long, global = true, env = "CLANKERD_BOOT_DIR_ROOT", hide = true)]
    boot_dir_root: bool,
    /// clankerd-vmspawn helper binary (default: next to vmctl).
    #[arg(long, global = true, env = "CLANKERD_VMSPAWN")]
    vmspawn: Option<PathBuf>,
    /// Static aarch64 Linux clankerd-guestd placed in the guest's boot
    /// directory (default: next to vmctl, or ../linux-arm64/ as in build/rust).
    #[arg(long, global = true, env = "CLANKERD_GUESTD")]
    guestd: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Pull an image (linux/arm64) into the local cache.
    Pull { image: String },
    /// List cached images.
    Images,
    /// Create a machine, start it, wait for it, and exit with its exit code.
    Run(RunArgs),
    /// Create a machine without starting it.
    Create(CreateArgs),
    /// Start created or exited machines.
    Start { machines: Vec<String> },
    /// Stop machines gracefully: the workload gets SIGTERM, the machine syncs,
    /// unmounts and powers off; it is killed after the timeout.
    Stop {
        /// Seconds to wait before killing.
        #[arg(short = 't', long = "time", default_value_t = 10)]
        time: u64,
        machines: Vec<String>,
    },
    /// Kill machines immediately (or send them a signal).
    Kill {
        /// Signal name or number.
        #[arg(short = 's', long, default_value = "KILL")]
        signal: String,
        machines: Vec<String>,
    },
    /// List machines.
    Ps {
        /// Show all machines, not only running ones.
        #[arg(short, long)]
        all: bool,
    },
    /// Show machine configuration and state as JSON.
    Inspect { machines: Vec<String> },
    /// Print the console output (kernel, guestd and workload) of a machine.
    Logs { machine: String },
    /// Run a command in a running machine.
    Exec(exec::ExecArgs),
    /// Remove machines.
    Rm {
        /// Remove running machines too.
        #[arg(short, long)]
        force: bool,
        machines: Vec<String>,
    },
}

#[derive(Args)]
struct RunArgs {
    /// Start the machine and print its id, without waiting for it to exit.
    #[arg(short = 'd', long)]
    detach: bool,
    #[command(flatten)]
    create: CreateArgs,
}

#[derive(Args)]
struct CreateArgs {
    #[arg(long)]
    name: Option<String>,
    /// Environment variable, KEY=value.
    #[arg(short = 'e', long = "env")]
    env: Vec<String>,
    #[arg(short = 'u', long)]
    user: Option<String>,
    #[arg(short = 'w', long)]
    workdir: Option<String>,
    #[arg(long)]
    entrypoint: Option<String>,
    /// Publish a guest port on host loopback: [IP:]HOST_PORT:GUEST_PORT.
    #[arg(short = 'p', long = "publish", value_parser = parse_publish)]
    publish: Vec<PortBinding>,
    image: String,
    /// Command and arguments.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    cmd: Vec<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("Error: {e}");
            ExitCode::from(125)
        }
    }
}

fn engine(cli: &Cli) -> Result<Engine, Error> {
    let defaults = EngineConfig::for_user();
    let mut cfg = EngineConfig::new(
        cli.state_dir.clone().unwrap_or(defaults.state_dir),
        cli.runtime_dir.clone().unwrap_or(defaults.runtime_dir),
    );
    cfg.insecure_registries = cli.insecure_registries.clone();
    // Real machines boot images, so the cache is on whenever libkrun is the
    // backend; the development stand-ins use it only when a cache directory
    // is given (their machines otherwise run host commands).
    let dev_local = std::env::var_os("CLANKERD_VMSPAWN_DEV_LOCAL").is_some_and(|v| v == "1");
    let libkrun = cli.dev_guestd.is_none()
        && (cfg!(target_os = "macos") || cli.vmspawn.is_some())
        && !dev_local
        && !cli.boot_dir_root;
    let wants_cache = matches!(cli.command, Command::Pull { .. } | Command::Images) || libkrun;
    if cli.cache_dir.is_some() || wants_cache {
        cfg.cache_dir = Some(
            cli.cache_dir
                .clone()
                .unwrap_or_else(EngineConfig::default_cache_dir),
        );
    }
    if let Some(guestd) = &cli.dev_guestd {
        cfg.vmm = Arc::new(LocalProcessVmm::new(guestd));
        let boot_dir = cli
            .boot_dir
            .clone()
            .or_else(|| guestd.parent().map(PathBuf::from))
            .unwrap_or_default();
        cfg.populator = Some(Arc::new(LocalGuestdPopulator::new(guestd, boot_dir)));
    } else if cfg!(target_os = "macos") || cli.vmspawn.is_some() {
        let vmspawn = cli
            .vmspawn
            .clone()
            .unwrap_or_else(|| sibling("clankerd-vmspawn", &["."]));
        let guestd = cli
            .guestd
            .clone()
            .unwrap_or_else(|| sibling("clankerd-guestd", &[".", "../linux-arm64"]));
        // Base root disks are built by a population boot of the same helper;
        // mke2fs and the other static tools sit next to guestd.
        let guest_dir = guestd.parent().map(PathBuf::from).unwrap_or_default();
        let mut populator = VmspawnPopulator::new(&vmspawn, guest_dir);
        let mut vmm = VmspawnVmm::new(vmspawn, guestd);
        // Development: CLANKERD_VMSPAWN_DEV_LOCAL=1 runs guestd as a local process, not a VM.
        if dev_local {
            vmm = vmm.dev_local();
            populator = populator.dev_local();
        }
        cfg.vmm = Arc::new(vmm);
        cfg.populator = Some(Arc::new(populator));
    }
    Engine::new(cfg)
}

/// Looks for `name` in directories relative to this executable; returns the
/// first candidate path (existing or not) so errors name a concrete path.
fn sibling(name: &str, dirs: &[&str]) -> PathBuf {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from))
        .unwrap_or_default();
    let candidates: Vec<PathBuf> = dirs.iter().map(|d| exe_dir.join(d).join(name)).collect();
    candidates
        .iter()
        .find(|p| p.exists())
        .or(candidates.first())
        .cloned()
        .unwrap_or_else(|| PathBuf::from(name))
}

fn create(engine: &Engine, a: CreateArgs) -> Result<libclankerd::Machine, Error> {
    let config = MachineConfig {
        image: a.image,
        entrypoint: a.entrypoint.into_iter().collect(),
        cmd: a.cmd,
        env: a.env,
        user: a.user.unwrap_or_default(),
        working_dir: a.workdir.unwrap_or_default(),
        ..Default::default()
    };
    let host_config = HostConfig {
        port_bindings: a.publish,
        ..Default::default()
    };
    engine.create(a.name.as_deref(), config, host_config)
}

/// Parses `[IP:]HOST_PORT:GUEST_PORT`. The library rejects non-loopback IPs.
fn parse_publish(s: &str) -> Result<PortBinding, String> {
    let port = |p: &str| {
        p.parse::<u16>()
            .map_err(|_| format!("invalid port \"{p}\""))
    };
    let parts: Vec<&str> = s.rsplitn(3, ':').collect();
    match parts.as_slice() {
        [guest, host] => Ok(PortBinding::loopback(port(host)?, port(guest)?)),
        [guest, host, ip] => Ok(PortBinding {
            host_ip: Some(ip.parse().map_err(|_| format!("invalid IP \"{ip}\""))?),
            host_port: port(host)?,
            guest_port: port(guest)?,
        }),
        _ => Err("expected [IP:]HOST_PORT:GUEST_PORT".into()),
    }
}

/// Runs `f` over every name, printing each success via `ok` and each error;
/// the exit code is 1 if any failed (like `docker rm a b`).
fn each(names: &[String], mut f: impl FnMut(&str) -> Result<String, Error>) -> u8 {
    let mut code = 0;
    for name in names {
        match f(name) {
            Ok(out) => println!("{out}"),
            Err(e) => {
                eprintln!("Error: {e}");
                code = 1;
            }
        }
    }
    code
}

fn run(cli: Cli) -> Result<u8, Error> {
    let engine = engine(&cli)?;
    match cli.command {
        Command::Pull { image } => {
            let info = engine.pull(&image)?;
            println!("Digest: {}", info.id);
            println!("Status: image is up to date for {image}");
            Ok(0)
        }
        Command::Images => {
            println!(
                "{:<40}{:<16}{:<14}{:<16}SIZE",
                "REPOSITORY", "TAG", "IMAGE ID", "PULLED"
            );
            for info in engine.images()? {
                print_image_rows(&info);
            }
            Ok(0)
        }
        Command::Run(args) => {
            let m = create(&engine, args.create)?;
            m.start()?;
            if args.detach {
                println!("{}", m.id());
                return Ok(0);
            }
            // Exit codes are 0..=255 on the host.
            Ok(m.wait()?.exit_code as u8)
        }
        Command::Exec(args) => exec::run(&engine, args),
        Command::Create(args) => {
            println!("{}", create(&engine, args)?.id());
            Ok(0)
        }
        Command::Start { machines } => Ok(each(&machines, |n| {
            engine.get(n)?.start()?;
            Ok(n.to_owned())
        })),
        Command::Stop { time, machines } => Ok(each(&machines, |n| {
            engine.get(n)?.stop(Duration::from_secs(time))?;
            Ok(n.to_owned())
        })),
        Command::Kill { signal, machines } => {
            let signal = parse_signal(&signal).map_err(Error::invalid_parameter)?;
            Ok(each(&machines, |n| {
                engine.get(n)?.kill(signal)?;
                Ok(n.to_owned())
            }))
        }
        Command::Ps { all } => {
            println!("{:<14}{:<24}{:<24}NAMES", "MACHINE ID", "IMAGE", "STATUS");
            for info in engine.list(all)? {
                println!(
                    "{:<14}{:<24}{:<24}{}",
                    &info.id[..12],
                    info.config.image,
                    status_text(&info),
                    info.name
                );
            }
            Ok(0)
        }
        Command::Inspect { machines } => {
            let mut infos = Vec::new();
            let mut code = 0;
            for n in &machines {
                match engine.get(n).and_then(|m| m.inspect()) {
                    Ok(i) => infos.push(i),
                    Err(e) => {
                        eprintln!("Error: {e}");
                        code = 1;
                    }
                }
            }
            println!("{}", serde_json::to_string_pretty(&infos).unwrap());
            Ok(code)
        }
        Command::Logs { machine } => {
            let bytes = engine.get(&machine)?.logs()?;
            std::io::stdout().write_all(&bytes).map_err(Error::from)?;
            Ok(0)
        }
        Command::Rm { force, machines } => Ok(each(&machines, |n| {
            engine.get(n)?.remove(force)?;
            Ok(n.to_owned())
        })),
    }
}

/// `KILL`, `SIGKILL`, `kill` or `9`.
fn parse_signal(s: &str) -> Result<i32, String> {
    if let Ok(n) = s.parse::<i32>() {
        return if (1..=64).contains(&n) {
            Ok(n)
        } else {
            Err(format!("invalid signal number {n}"))
        };
    }
    let name = s.to_ascii_uppercase();
    let name = name.strip_prefix("SIG").unwrap_or(&name);
    Ok(match name {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "KILL" => libc::SIGKILL,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "TERM" => libc::SIGTERM,
        "CONT" => libc::SIGCONT,
        "STOP" => libc::SIGSTOP,
        _ => return Err(format!("unknown signal \"{s}\"")),
    })
}

fn status_text(info: &MachineInfo) -> String {
    match info.state.status {
        Status::Exited => format!("Exited ({})", info.state.exit_code),
        Status::Running => "Up".into(),
        other => {
            let s = other.to_string();
            s[..1].to_uppercase() + &s[1..]
        }
    }
}

/// One row per reference (or `<none>` for an image whose tag moved on).
fn print_image_rows(info: &ImageInfo) {
    let id = info.id.trim_start_matches("sha256:");
    let id = &id[..id.len().min(12)];
    let refs: Vec<(String, String)> = if info.references.is_empty() {
        vec![("<none>".into(), "<none>".into())]
    } else {
        info.references.iter().map(|r| split_reference(r)).collect()
    };
    for (repo, tag) in refs {
        println!(
            "{:<40}{:<16}{:<14}{:<16}{}",
            repo,
            tag,
            id,
            age_text(info.pulled),
            size_text(info.size())
        );
    }
}

/// `host/name:tag` -> (`host/name`, `tag`); digest references show their digest as the tag.
fn split_reference(r: &str) -> (String, String) {
    if let Some((name, digest)) = r.split_once('@') {
        let name = name
            .rsplit_once(':')
            .filter(|(_, t)| !t.contains('/'))
            .map_or(name, |(n, _)| n);
        return (name.to_owned(), digest.to_owned());
    }
    match r.rsplit_once(':') {
        Some((name, tag)) if !tag.contains('/') => (name.to_owned(), tag.to_owned()),
        _ => (r.to_owned(), "latest".to_owned()),
    }
}

fn age_text(t: std::time::SystemTime) -> String {
    let secs = t.elapsed().map(|d| d.as_secs()).unwrap_or(0);
    match secs {
        0..=59 => "seconds ago".into(),
        60..=3599 => format!("{} minutes ago", secs / 60),
        3600..=86399 => format!("{} hours ago", secs / 3600),
        _ => format!("{} days ago", secs / 86400),
    }
}

fn size_text(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "kB", "MB", "GB"];
    let (mut v, mut u) = (bytes as f64, 0);
    while v >= 1000.0 && u < UNITS.len() - 1 {
        v /= 1000.0;
        u += 1;
    }
    format!("{v:.1}{}", UNITS[u])
}
