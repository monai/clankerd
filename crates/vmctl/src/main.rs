//! vmctl: Docker-style CLI over libclankerd.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

mod exec;
mod parse;

use contract::results::*;
use contract::*;
use libclankerd::net::VmnetBackend;
use libclankerd::vmm::{LocalProcessVmm, VmspawnVmm};
use libclankerd::{
    Engine, EngineConfig, Error, HostConfig, LocalGuestdPopulator, MachineConfig, VmspawnPopulator,
};
mod contract;

fn main() -> ExitCode {
    let (cli, output) = contract::invocation();
    let image_pull = matches!(cli.command, Command::Image(ImageCommand::Pull { .. }));
    match run(cli, output) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            output.diagnostic(None, &e);
            if output == Output::Json {
                if image_pull {
                    output.json(&Option::<ImageRecord>::None);
                } else {
                    output.json(&Vec::<Operation>::new());
                }
            }
            ExitCode::from(1)
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
    let wants_cache = matches!(cli.command, Command::Image(_)) || libkrun;
    if cli.cache_dir.is_some() || wants_cache {
        cfg.cache_dir = Some(
            cli.cache_dir
                .clone()
                .unwrap_or_else(EngineConfig::default_cache_dir),
        );
    }
    if let Some(guestd) = &cli.dev_guestd {
        cfg.vmm = Arc::new(LocalProcessVmm::new(guestd));
        if let Some(root) = &cli.guest_root {
            cfg.populator = Some(Arc::new(LocalGuestdPopulator::new(guestd, root)));
        }
    } else if cfg!(target_os = "macos") || cli.vmspawn.is_some() {
        let vmspawn = cli
            .vmspawn
            .clone()
            .unwrap_or_else(|| sibling("clankerd-vmspawn"));
        let guest_root = cli
            .guest_root
            .clone()
            .unwrap_or_else(|| sibling("../linux-arm64/management"));
        let mut populator = VmspawnPopulator::new(&vmspawn, &guest_root);
        let mut vmm = VmspawnVmm::new(vmspawn, guest_root);
        // Development: CLANKERD_VMSPAWN_DEV_LOCAL=1 runs guestd as a local process, not a VM.
        if dev_local {
            vmm = vmm.dev_local();
            populator = populator.dev_local();
        }
        cfg.vmm = Arc::new(vmm);
        cfg.populator = Some(Arc::new(populator));
        if !dev_local && !cli.boot_dir_root {
            cfg.net = Some(Arc::new(match &cli.network_executable {
                Some(binary) => VmnetBackend::new(binary),
                None => VmnetBackend::default(),
            }));
        }
    }
    Engine::new(cfg)
}

/// Resolve one artifact-relative executable path; no host or former-layout search.
fn sibling(relative: &str) -> PathBuf {
    std::env::current_exe()
        .expect("current executable path")
        .parent()
        .expect("executable directory")
        .join(relative)
}

fn create(engine: &Engine, a: CreateArgs) -> Result<libclankerd::Machine, Error> {
    let config = MachineConfig {
        image: a.image,
        entrypoint: a.entrypoint.into_iter().collect(),
        cmd: a.cmd,
        env: a.env,
        user: a.user.unwrap_or_default(),
        working_dir: a.workdir.unwrap_or_default(),
        tty: a.tty,
        open_stdin: a.interactive,
        stop_signal: a.stop_signal.unwrap_or_default(),
    };
    let mut host = HostConfig {
        mounts: a.mount,
        socket_bindings: a.socket,
        ..Default::default()
    };
    a.host.apply(engine, &mut host)?;
    engine.create(a.name.as_deref(), config, host)
}

fn run(cli: Cli, output: Output) -> Result<u8, Error> {
    let engine = engine(&cli)?;
    match cli.command {
        Command::Machine(command) => machine(&engine, *command, output),
        Command::Image(ImageCommand::Pull { image }) => {
            let info = ImageRecord::from(engine.pull(&image)?);
            if output == Output::Json {
                output.json(&info);
            } else {
                println!("{}", info.id);
            }
            Ok(0)
        }
        Command::Image(ImageCommand::Ls) => {
            let records: Vec<ImageRecord> = engine.images()?.into_iter().map(Into::into).collect();
            output.table(&records);
            Ok(0)
        }
        Command::Volume(cmd) => volume(&engine, cmd, output),
        Command::Network(cmd) => network(&engine, cmd, output),
    }
}

fn machine(engine: &Engine, command: MachineCommand, output: Output) -> Result<u8, Error> {
    match command {
        MachineCommand::Run(args) => {
            let tty = args.create.tty;
            if tty && !args.detach && !exec::isatty(libc::STDIN_FILENO) {
                return Err(Error::invalid_parameter("the input device is not a TTY"));
            }
            if args.detach {
                let target = args
                    .create
                    .name
                    .clone()
                    .unwrap_or_else(|| args.create.image.clone());
                return Ok(output.batch(&[target], |_| {
                    let m = create(engine, args.create.clone())?;
                    m.start()?;
                    Ok(m.id().into())
                }));
            }
            let m = create(engine, args.create)?;
            let streams = m.start_attached(if tty { exec::window_size() } else { None })?;
            let resize = streams.resizer();
            let status = exec::drive(streams, tty, move |rows, cols| {
                let _ = resize(rows, cols);
            })?;
            Ok(status.exit_code as u8)
        }
        MachineCommand::Exec(args) => exec::run(engine, args),
        MachineCommand::Create(args) => {
            let target = args.name.clone().unwrap_or_else(|| args.image.clone());
            Ok(output.batch(&[target], |_| {
                create(engine, args.clone()).map(|m| m.id().to_owned())
            }))
        }
        MachineCommand::Start { machines } => Ok(output.batch(&machines, |n| {
            engine.get(n)?.start()?;
            Ok(n.to_owned())
        })),
        MachineCommand::Update(args) => Ok(output.batch(&args.machines, |name| {
            let machine = engine.get(name)?;
            let info = machine.inspect()?;
            let mut host = info.host_config;
            args.apply(engine, &mut host)?;
            machine.update(&info.config, host)?;
            Ok(name.to_owned())
        })),
        MachineCommand::Reimage { machine, image } => Ok(output.batch(&[machine], |n| {
            engine.get(n)?.change_image(&image)?;
            Ok(n.to_owned())
        })),
        MachineCommand::Stop { time, machines } => Ok(output.batch(&machines, |n| {
            engine.get(n)?.stop(Duration::from_secs(time))?;
            Ok(n.to_owned())
        })),
        MachineCommand::Kill { machines } => Ok(output.batch(&machines, |n| {
            engine.get(n)?.kill(libc::SIGKILL)?;
            Ok(n.to_owned())
        })),
        MachineCommand::Ps { all } => {
            let records: Vec<MachineRecord> =
                engine.list(all)?.into_iter().map(Into::into).collect();
            output.table(&records);
            Ok(0)
        }
        MachineCommand::Inspect { machines } => Ok(output.inspect(&machines, |n| {
            engine.get(n)?.inspect().map(MachineRecord::from)
        })),
        MachineCommand::Logs { machine } => {
            std::io::stdout()
                .write_all(&engine.get(&machine)?.logs()?)
                .map_err(Error::from)?;
            Ok(0)
        }
        MachineCommand::Rm { force, machines } => Ok(output.batch(&machines, |n| {
            engine.get(n)?.remove(force)?;
            Ok(n.to_owned())
        })),
    }
}

fn network(engine: &Engine, cmd: NetworkCommand, output: Output) -> Result<u8, Error> {
    match cmd {
        NetworkCommand::Ls => {
            let records: Vec<NetworkRecord> =
                engine.networks()?.into_iter().map(Into::into).collect();
            output.table(&records);
            Ok(0)
        }
        NetworkCommand::Create { name, mode, subnet } => Ok(output.batch(&[name], |n| {
            engine
                .create_network(n, mode, subnet.clone())
                .map(|n| n.name)
        })),
        NetworkCommand::Inspect { names } => Ok(output.inspect(&names, |n| {
            engine.inspect_network(n).map(NetworkRecord::from)
        })),
        NetworkCommand::Rm { names } => Ok(output.batch(&names, |n| {
            engine.remove_network(n)?;
            Ok(n.to_owned())
        })),
    }
}

fn volume(engine: &Engine, cmd: VolumeCommand, output: Output) -> Result<u8, Error> {
    match cmd {
        VolumeCommand::Ls => {
            let records: Vec<VolumeRecord> =
                engine.volumes()?.into_iter().map(Into::into).collect();
            output.table(&records);
            Ok(0)
        }
        VolumeCommand::Create { size, name } => {
            Ok(output.batch(&[name], |n| engine.create_volume(n, size).map(|v| v.name)))
        }
        VolumeCommand::Inspect { names } => {
            Ok(output.inspect(&names, |n| engine.volume(n).map(VolumeRecord::from)))
        }
        VolumeCommand::Rm { names } => Ok(output.batch(&names, |n| {
            engine.remove_volume(n)?;
            Ok(n.to_owned())
        })),
    }
}
