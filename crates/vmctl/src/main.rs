//! vmctl: Docker-style CLI over libclankerd.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

mod exec;

use clap::{Args, Parser, Subcommand};
use libclankerd::vmm::LocalProcessVmm;
use libclankerd::{Engine, EngineConfig, Error, HostConfig, MachineConfig, MachineInfo, Status};

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
    /// instead of booting a VM (no libkrun backend is wired up yet).
    #[arg(long, global = true, env = "CLANKERD_DEV_GUESTD", hide = true)]
    dev_guestd: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a machine, start it, wait for it, and exit with its exit code.
    Run(CreateArgs),
    /// Create a machine without starting it.
    Create(CreateArgs),
    /// Start created or exited machines.
    Start { machines: Vec<String> },
    /// List machines.
    Ps {
        /// Show all machines, not only running ones.
        #[arg(short, long)]
        all: bool,
    },
    /// Show machine configuration and state as JSON.
    Inspect { machines: Vec<String> },
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
    if let Some(guestd) = &cli.dev_guestd {
        cfg.vmm = Arc::new(LocalProcessVmm::new(guestd));
    }
    Engine::new(cfg)
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
    engine.create(a.name.as_deref(), config, HostConfig::default())
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
        Command::Run(args) => {
            let m = create(&engine, args)?;
            m.start()?;
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
        Command::Rm { force, machines } => Ok(each(&machines, |n| {
            engine.get(n)?.remove(force)?;
            Ok(n.to_owned())
        })),
    }
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
