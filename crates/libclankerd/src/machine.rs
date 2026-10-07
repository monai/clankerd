//! `Machine`: a handle on one machine, plus the start/monitor lifecycle.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use clankerd_proto::guest::{
    AttachParams, Clock, Event, METHOD_ATTACH, METHOD_KILL, METHOD_SET_CLOCK, METHOD_SHUTDOWN,
    NetworkConfig, ResizeParams, ShutdownParams, SignalParams, Workload,
};
use serde::Serialize;

use crate::config::{HostConfig, MachineConfig, PortBinding};
use crate::engine::Inner;
use crate::error::{Error, ErrorKind, Result};
use crate::events::EventAction;
use crate::exec::{Exec, ExecConfig, ExecStreams};
use crate::guest::{self, EventStream, Next};
use crate::image_config::ImageConfig;
use crate::state::{MachineState, Status, WaitResult};
use crate::tunnel::{GuestBinding, MachineTunnels, PublishedPort, validate_port_binding};
use crate::vmm::BootSpec;
use crate::volumes::VolumeStore;

/// Everything `inspect` reports: the creation-time record plus current state.
#[derive(Debug, Clone, Serialize)]
pub struct MachineInfo {
    pub id: String,
    pub name: String,
    pub created: SystemTime,
    pub config: MachineConfig,
    pub host_config: HostConfig,
    /// Digest the image was pinned to when the machine was created.
    pub image_id: String,
    /// The image's runtime defaults the configuration above was merged over.
    pub image_config: Option<ImageConfig>,
    pub state: MachineState,
}

/// File in the machine directory that receives the console output.
pub const CONSOLE_LOG: &str = "console.log";

/// A handle on a machine. Cheap to clone; dropping it never stops the machine.
#[derive(Clone)]
pub struct Machine {
    inner: Arc<Inner>,
    id: String,
}

impl Machine {
    pub(crate) fn new(inner: Arc<Inner>, id: String) -> Self {
        Machine { inner, id }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn inspect(&self) -> Result<MachineInfo> {
        let (record, state) = self.inner.store.load(&self.id)?;
        Ok(MachineInfo {
            id: record.id,
            name: record.name,
            created: record.created,
            config: record.config,
            host_config: record.host_config,
            image_id: record.image_id,
            image_config: record.image_config,
            state,
        })
    }

    /// Boots the machine and returns once the guest reported ready.
    /// Fails with `Unavailable` if it does not within the engine's start timeout.
    pub fn start(&self) -> Result<()> {
        self.start_with(false)
    }

    /// `restart` is set when a restart policy starts the machine: it only
    /// proceeds while the machine is still waiting for that restart.
    fn start_with(&self, restart: bool) -> Result<()> {
        let info = self.inspect()?;
        {
            let mut starting = self.inner.lock();
            let status = self.inner.store.load_state(&self.id)?.status;
            let busy = status == Status::Running
                || starting.contains(&self.id)
                || (!restart && status == Status::Restarting);
            // A restart whose machine was stopped or removed meanwhile is off.
            let cancelled = restart && status != Status::Restarting;
            if busy || cancelled {
                return Err(Error::conflict(format!(
                    "machine {} is already running",
                    info.name
                )));
            }
            if !restart {
                self.inner.stopped().remove(&self.id);
            }
            if let Some(name) = crate::mount::volume_name(&info.host_config.mounts) {
                self.check_volume_free(&starting, name)?;
            }
            starting.insert(self.id.clone());
        }
        let result = self.boot(&info, restart);
        self.inner.lock().remove(&self.id);
        self.inner.changed.notify_all();
        result
    }

    /// A volume is one filesystem: only one machine may run on it at a time.
    fn check_volume_free(
        &self,
        starting: &std::collections::HashSet<String>,
        volume: &str,
    ) -> Result<()> {
        for id in self.inner.store.ids()? {
            if id == self.id {
                continue;
            }
            let Ok((record, state)) = self.inner.store.load(&id) else {
                continue;
            };
            let busy = state.status == Status::Running || starting.contains(&id);
            if busy && crate::mount::volume_name(&record.host_config.mounts) == Some(volume) {
                return Err(Error::conflict(format!(
                    "volume \"{volume}\" is in use by running machine {}",
                    record.name
                )));
            }
        }
        Ok(())
    }

    fn boot(&self, info: &MachineInfo, restart: bool) -> Result<()> {
        let dir = self.inner.store.machine_dir(&self.id);
        let exit_file = dir.join("exit");
        let socket = self.inner.socket_path(&self.id);
        let _ = fs::remove_file(&exit_file);

        let root_disk = Some(dir.join(crate::engine::ROOT_DISK)).filter(|d| d.exists());
        // Grows the volume file if its size increased; binds are checked here.
        let plan = crate::mount::plan(
            &VolumeStore::new(self.inner.store.volumes_dir()),
            &info.host_config.mounts,
            root_disk.is_some(),
        )?;
        let mut argv = info.config.entrypoint.clone();
        argv.extend(info.config.cmd.iter().cloned());
        let net = match &self.inner.net {
            Some(backend) => Some(backend.start(&self.inner.net_spec(&self.id)).inspect_err(
                |err| {
                    let _ = self
                        .inner
                        .update_state(&self.id, |s| s.error = err.message().to_owned());
                },
            )?),
            None => None,
        };
        let spec = BootSpec {
            machine_id: self.id.clone(),
            dir,
            root_disk,
            guest_socket: socket.clone(),
            host_socket: self.inner.host_socket_path(&self.id),
            exit_file: exit_file.clone(),
            workload: Workload {
                argv,
                env: info.config.env.clone(),
                working_dir: info.config.working_dir.clone(),
                user: info.config.user.clone(),
                clock: Some(Clock::now()),
                mounts: plan.guest,
                network: net.as_ref().map(|n| NetworkConfig {
                    interface: "eth0".into(),
                    hostname: self.id[..12].to_owned(),
                    blocked: n.blocked.clone(),
                }),
                tty: info.config.tty,
                open_stdin: info.config.open_stdin,
            },
            cpus: info.host_config.cpus,
            memory: info.host_config.memory,
            volume_disk: plan.volume_disk,
            shares: plan.shares,
            net,
        };

        let fail = |err: Error| {
            let _ = self
                .inner
                .update_state(&self.id, |s| s.error = err.message().to_owned());
            err
        };
        let handle = self.inner.vmm.boot(&spec).map_err(|err| {
            self.inner.stop_net(&self.id);
            fail(err)
        })?;
        let alive = || self.inner.vmm.check_alive(&spec, &handle);
        let ready = match wait_ready(&socket, &exit_file, self.inner.start_timeout, &alive) {
            Ok(r) => r,
            Err(err) => {
                kill_group(handle.pid);
                self.inner.stop_net(&self.id);
                let _ = fs::remove_file(&socket);
                return Err(fail(err));
            }
        };

        if matches!(ready, Ready::Stream(_))
            && let Err(err) = start_tunnels(&self.inner, &self.id, &info.host_config, true)
        {
            kill_group(handle.pid);
            self.inner.stop_net(&self.id);
            self.inner.drop_tunnels(&self.id);
            let _ = fs::remove_file(&socket);
            return Err(fail(err));
        }
        self.inner.update_state(&self.id, |s| {
            s.status = Status::Running;
            s.pid = Some(handle.pid);
            s.error.clear();
            s.exit_code = 0;
            s.started_at = Some(SystemTime::now());
            s.finished_at = None;
            if !restart {
                s.restart_count = 0;
            }
        })?;
        self.inner.emit(&self.id, EventAction::Started, None);
        spawn_monitor(self.inner.clone(), self.id.clone(), ready);
        Ok(())
    }

    /// Replaces the host configuration of a machine that is not running:
    /// resources, mounts, ports and policies. `config` must equal the
    /// machine's current [`MachineConfig`]: it is immutable, and a difference
    /// fails with `InvalidParameter` naming each field that cannot change
    /// (use [`Machine::change_image`] to move to another image). Nothing is
    /// applied on failure.
    pub fn update(&self, config: &MachineConfig, host_config: HostConfig) -> Result<()> {
        crate::tunnel::validate_host_config(&host_config)?;
        crate::mount::validate(&host_config.mounts)?;
        let starting = self.inner.lock();
        let (mut record, state) = self.inner.store.load(&self.id)?;
        let immutable = record.config.differing_fields(config);
        if !immutable.is_empty() {
            return Err(Error::invalid_parameter(format!(
                "cannot update immutable fields: {}",
                immutable.join(", ")
            )));
        }
        if matches!(state.status, Status::Running | Status::Restarting)
            || starting.contains(&self.id)
        {
            return Err(Error::conflict(format!(
                "cannot update machine {}: it is running; stop it first",
                record.name
            )));
        }
        crate::mount::ensure_volume(
            &VolumeStore::new(self.inner.store.volumes_dir()),
            &host_config.mounts,
        )?;
        record.host_config = host_config;
        self.inner.store.save_record(&record)?;
        self.inner
            .emit_named(&self.id, record.name, EventAction::Updated, None);
        Ok(())
    }

    /// Moves a machine that is not running to another image: the root disk is
    /// rebuilt from the new image's cached base (pulled per the machine's pull
    /// policy) and what the machine runs is merged again over the new image's
    /// defaults. The volume is untouched.
    pub fn change_image(&self, reference: &str) -> Result<()> {
        if reference.is_empty() {
            return Err(Error::invalid_parameter("image is required"));
        }
        let host_config = self.inspect()?.host_config;
        // Pulling and building can take minutes: before taking the lock.
        let (image, defaults, base) = self
            .inner
            .resolve_image(reference, host_config.pull_policy)?;
        let starting = self.inner.lock();
        let (mut record, state) = self.inner.store.load(&self.id)?;
        if matches!(state.status, Status::Running | Status::Restarting)
            || starting.contains(&self.id)
        {
            return Err(Error::conflict(format!(
                "cannot change the image of machine {}: it is running; stop it first",
                record.name
            )));
        }
        let mut requested = record
            .requested
            .clone()
            .unwrap_or_else(|| record.config.clone());
        requested.image = reference.to_owned();
        let config = defaults.merge(&requested)?;
        // Swap the disk in atomically, so a failure leaves the old one.
        let dir = self.inner.store.machine_dir(&self.id);
        let part = dir.join(format!("{}.new", crate::engine::ROOT_DISK));
        let _ = fs::remove_file(&part);
        crate::rootdisk::clone_file(&base, &part)?;
        if let Err(e) = fs::rename(&part, dir.join(crate::engine::ROOT_DISK)) {
            let _ = fs::remove_file(&part);
            return Err(e.into());
        }
        record.config = config;
        record.requested = Some(requested);
        record.image_id = image.id;
        record.image_config = Some(defaults);
        self.inner.store.save_record(&record)?;
        self.inner
            .emit_named(&self.id, record.name, EventAction::Updated, None);
        Ok(())
    }

    /// Sets the guest's clock from the host's. The engine does this by itself
    /// when the host wakes from sleep; it is public for callers that know
    /// better. Fails with a conflict if the machine is not running.
    pub fn sync_clock(&self) -> Result<()> {
        let (record, state) = self.inner.store.load(&self.id)?;
        if state.status != Status::Running {
            return Err(Error::conflict(format!(
                "machine {} is not running",
                record.name
            )));
        }
        guest::call(
            &self.inner.socket_path(&self.id),
            METHOD_SET_CLOCK,
            &Clock::now(),
        )
        .map(|_| ())
    }

    /// Registers a command to run inside the running machine (Docker's exec create).
    pub fn exec_create(&self, config: ExecConfig) -> Result<Exec> {
        let (record, state) = self.inner.store.load(&self.id)?;
        if state.status != Status::Running {
            return Err(Error::conflict(format!(
                "machine {} is not running",
                record.name
            )));
        }
        Exec::create(self.inner.socket_path(&self.id), config)
    }

    /// Attaches to the machine's main process, like `docker attach`: its
    /// terminal (`tty`) or stdio (`open_stdin`) become the returned streams.
    /// Output from before the attach is replayed. Dropping the streams detaches
    /// without stopping the machine; the status arrives when the process exits.
    /// `size` is the terminal size `(rows, cols)` for a machine with a tty.
    pub fn attach(&self, size: Option<(u16, u16)>) -> Result<ExecStreams> {
        let (record, state) = self.inner.store.load(&self.id)?;
        if state.status != Status::Running {
            return Err(Error::conflict(format!(
                "machine {} is not running",
                record.name
            )));
        }
        if !record.config.tty && !record.config.open_stdin {
            return Err(Error::conflict(format!(
                "machine {} was created without tty or open_stdin, so there is nothing to attach to",
                record.name
            )));
        }
        let reader = guest::upgrade(
            &self.inner.socket_path(&self.id),
            METHOD_ATTACH,
            &AttachParams {
                size: size.map(|(rows, cols)| ResizeParams {
                    id: String::new(),
                    rows,
                    cols,
                }),
            },
        )?;
        ExecStreams::open(reader, record.config.open_stdin)
    }

    fn tunnels(&self) -> Result<Arc<MachineTunnels>> {
        self.inner
            .tunnels()
            .get(&self.id)
            .cloned()
            .ok_or_else(|| Error::conflict("machine is not running"))
    }

    /// Publishes a guest port on host loopback while the machine runs.
    /// Dropping the returned handle closes the listener.
    ///
    /// Known limitation: the listeners live in the calling process. The machine
    /// survives that process exiting, but its published ports (and host-gateway
    /// ports and socket bindings) do not; a new process gets them back only by
    /// restarting the machine.
    pub fn publish(&self, binding: PortBinding) -> Result<PublishedPort> {
        validate_port_binding(&binding)?;
        self.tunnels()?.publish(&binding)
    }

    /// Makes host loopback `port` reachable at the same port on the guest's
    /// loopback. Dropping the handle removes it.
    pub fn expose_host_port(&self, port: u16) -> Result<GuestBinding> {
        if port == 0 {
            return Err(Error::invalid_parameter("host-gateway port must not be 0"));
        }
        self.tunnels()?.expose_host_port(port)
    }

    /// Exposes the host unix socket `host_path` at `guest_path` inside the guest.
    /// Dropping the handle removes it.
    pub fn bind_socket(
        &self,
        guest_path: impl AsRef<Path>,
        host_path: impl AsRef<Path>,
    ) -> Result<GuestBinding> {
        let (g, h) = (guest_path.as_ref(), host_path.as_ref());
        if !g.is_absolute() || !h.is_absolute() {
            return Err(Error::invalid_parameter(
                "socket binding paths must be absolute",
            ));
        }
        self.tunnels()?.bind_socket(g, h)
    }

    /// Blocks until the machine is not running and returns its exit code.
    /// Returns immediately for a machine that never started.
    pub fn wait(&self) -> Result<WaitResult> {
        Ok(self
            .wait_until(None, true)?
            .expect("waiting without a deadline ends with a result"))
    }

    /// Like [`Machine::wait`], giving up with `None` at `deadline`.
    ///
    /// With `settled` it also waits out restarts: the machine is done only
    /// when no restart policy is about to start it again. Without, a run ending
    /// is enough.
    fn wait_until(&self, deadline: Option<Instant>, settled: bool) -> Result<Option<WaitResult>> {
        let mut starting = self.inner.lock();
        loop {
            let state = self.inner.store.load_state(&self.id)?;
            let over =
                state.status != Status::Running && !(settled && state.status == Status::Restarting);
            if over && !starting.contains(&self.id) {
                return Ok(Some(WaitResult {
                    exit_code: state.exit_code,
                }));
            }
            let mut nap = Duration::from_millis(500);
            if let Some(deadline) = deadline {
                match deadline.checked_duration_since(Instant::now()) {
                    Some(left) if !left.is_zero() => nap = nap.min(left),
                    _ => return Ok(None),
                }
            }
            starting = self
                .inner
                .changed
                .wait_timeout(starting, nap)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Stops the machine gracefully: guestd signals the workload (the stop
    /// signal of its config, SIGTERM by default),
    /// and once it exits stops the remaining processes, syncs, unmounts and
    /// powers off. If the machine is still running after `timeout` (or guestd
    /// cannot be reached) the VMM is killed. Stopping a machine that is not
    /// running succeeds.
    pub fn stop(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        let (record, state) = self.inner.store.load(&self.id)?;
        match state.status {
            Status::Running => {}
            Status::Restarting => {
                // Waiting for a restart: stopping cancels it.
                self.inner.update_state(&self.id, |s| {
                    if s.status == Status::Restarting {
                        s.status = Status::Exited;
                    }
                })?;
                return Ok(());
            }
            _ => return Ok(()),
        }
        self.inner.stopped().insert(self.id.clone());
        let signal = Some(record.config.stop_signal.as_str())
            .filter(|s| !s.is_empty())
            .and_then(|s| crate::signal::parse_signal(s).ok())
            .unwrap_or(libc::SIGTERM);
        let asked = guest::call(
            &self.inner.socket_path(&self.id),
            METHOD_SHUTDOWN,
            &ShutdownParams { signal },
        );
        if asked.is_ok() && self.wait_until(Some(deadline), false)?.is_some() {
            return Ok(());
        }
        match self.kill(libc::SIGKILL) {
            // It ended between the check and the kill.
            Err(e) if e.kind() == ErrorKind::Conflict => Ok(()),
            other => other,
        }
    }

    /// Sends `signal` to the machine, like `docker kill`. SIGKILL ends the VMM
    /// at once and records exit code 137; any other signal is delivered to the
    /// workload by guestd. Fails with a conflict if the machine is not running.
    pub fn kill(&self, signal: i32) -> Result<()> {
        let (record, state) = self.inner.store.load(&self.id)?;
        let not_running = || {
            Error::conflict(format!(
                "cannot kill machine {}: it is not running",
                record.name
            ))
        };
        let pid = match (state.status, state.pid) {
            (Status::Running, Some(pid)) => pid,
            _ => return Err(not_running()),
        };
        if signal != libc::SIGKILL {
            return guest::call(
                &self.inner.socket_path(&self.id),
                METHOD_KILL,
                &SignalParams {
                    id: String::new(),
                    signal,
                },
            )
            .map(|_| ());
        }
        self.inner.emit(&self.id, EventAction::Killed, None);
        self.inner.forced().insert(self.id.clone());
        kill_group(pid);
        // The monitor records the end; return once it has.
        self.wait_until(Some(Instant::now() + Duration::from_secs(10)), false)?;
        self.inner.forced().remove(&self.id);
        Ok(())
    }

    /// Everything the machine printed on its console (kernel, guestd and
    /// workload output) since it was created. Empty before the first start.
    pub fn logs(&self) -> Result<Vec<u8>> {
        // Existence check first so unknown ids report NotFound.
        self.inspect()?;
        match fs::read(self.inner.store.machine_dir(&self.id).join(CONSOLE_LOG)) {
            Ok(bytes) => Ok(bytes),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(e.into()),
        }
    }

    /// Like [`Machine::remove`], and deletes the machine's named volume too.
    /// Refuses (removing nothing) while another machine uses the volume.
    pub fn remove_with_volumes(&self, force: bool) -> Result<()> {
        let info = self.inspect()?;
        let Some(volume) = crate::mount::volume_name(&info.host_config.mounts) else {
            return self.remove(force);
        };
        if let Some(other) = crate::volumes::users_of(&self.inner, volume, Some(&self.id))?.first()
        {
            return Err(Error::conflict(format!(
                "volume \"{volume}\" is also used by machine {other}"
            )));
        }
        self.remove(force)?;
        match VolumeStore::new(self.inner.store.volumes_dir()).remove(volume) {
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }

    /// Deletes the machine and its state, keeping its named volume. A running
    /// machine needs `force`.
    pub fn remove(&self, force: bool) -> Result<()> {
        let mut starting = self.inner.lock();
        let (record, state) = self.inner.store.load(&self.id)?;
        if starting.contains(&self.id) {
            return Err(Error::conflict(format!(
                "machine {} is starting",
                record.name
            )));
        }
        if matches!(state.status, Status::Running | Status::Restarting) {
            if !force {
                return Err(Error::conflict(format!(
                    "cannot remove running machine {}: stop it first or use force",
                    record.name
                )));
            }
            if let Some(pid) = state.pid {
                kill_group(pid);
            }
        }
        self.inner.stop_net(&self.id);
        self.inner.drop_tunnels(&self.id);
        self.inner.store.remove(&self.id)?;
        self.inner
            .emit_named(&self.id, record.name, EventAction::Removed, None);
        let _ = fs::remove_file(self.inner.socket_path(&self.id));
        starting.remove(&self.id);
        self.inner.changed.notify_all();
        Ok(())
    }
}

/// Outcome of waiting for guestd after boot.
enum Ready {
    Stream(EventStream),
    /// The workload already ended; its exit code was recovered.
    Exited(i32),
}

fn read_exit_file(path: &Path) -> Option<i32> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn wait_ready(
    socket: &Path,
    exit_file: &Path,
    timeout: Duration,
    alive: &dyn Fn() -> Result<()>,
) -> Result<Ready> {
    let deadline = Instant::now() + timeout;
    let timed_out = || {
        Error::unavailable(format!(
            "timed out after {timeout:?} waiting for the guest's ready event"
        ))
    };
    loop {
        if let Ok(mut stream) = EventStream::connect(socket) {
            match stream.next(Some(deadline)) {
                Next::Event(Event::Ready) => return Ok(Ready::Stream(stream)),
                Next::Event(Event::Exited { exit_code }) => return Ok(Ready::Exited(exit_code)),
                Next::TimedOut => return Err(timed_out()),
                Next::Closed => {}
            }
        }
        if let Some(code) = read_exit_file(exit_file) {
            return Ok(Ready::Exited(code));
        }
        alive()?;
        if Instant::now() >= deadline {
            return Err(timed_out());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Follows the guest's events until the workload exits, then records the result.
fn spawn_monitor(inner: Arc<Inner>, id: String, ready: Ready) {
    std::thread::spawn(move || {
        let exit_file = inner.store.machine_dir(&id).join("exit");
        let code = match ready {
            Ready::Exited(code) => Some(code),
            Ready::Stream(mut stream) => loop {
                match stream.next(None) {
                    Next::Event(Event::Exited { exit_code }) => break Some(exit_code),
                    Next::Event(Event::Ready) => {}
                    Next::Closed | Next::TimedOut => break read_exit_file(&exit_file),
                }
            },
        };
        record_exit(&inner, &id, code);
    });
}

/// Persists the end of a run. `None` means the guest vanished: status `dead`.
/// When the machine's restart policy asks for it the machine goes to
/// `restarting` instead and a restart is scheduled.
fn record_exit(inner: &Arc<Inner>, id: &str, code: Option<i32>) {
    // Release host resources first: whoever sees the new state sees them gone.
    inner.drop_tunnels(id);
    inner.stop_net(id);
    let _ = fs::remove_file(inner.socket_path(id));
    // A machine whose helper we killed ended by SIGKILL: Docker's 137.
    let code = code.or_else(|| inner.forced().remove(id).then_some(137));
    let policy = inner
        .store
        .load(id)
        .map(|(r, _)| r.host_config.restart_policy)
        .unwrap_or_default();
    let mut restart_in = None;
    let _ = inner.update_state(id, |s| {
        if s.status != Status::Running {
            return;
        }
        // Emitted under the state lock: whoever sees the new state (a waiter
        // that then removes the machine, say) sees this event first.
        inner.emit(id, EventAction::Exited, Some(code.unwrap_or(-1)));
        s.status = if code.is_some() {
            Status::Exited
        } else {
            Status::Dead
        };
        s.exit_code = code.unwrap_or(-1);
        s.pid = None;
        let now = SystemTime::now();
        s.finished_at = Some(now);
        let stopped_by_api = inner.stopped().remove(id);
        if !stopped_by_api && policy.restarts(s.exit_code, s.restart_count) {
            // Docker's backoff: the delay doubles while runs are short.
            let ran = s
                .started_at
                .and_then(|t| now.duration_since(t).ok())
                .unwrap_or_default();
            let doublings = if ran >= STABLE_RUN {
                0
            } else {
                s.restart_count.min(9)
            };
            restart_in =
                Some((inner.restart_delay * 2u32.pow(doublings)).min(Duration::from_secs(60)));
            s.status = Status::Restarting;
            s.restart_count += 1;
            inner.emit(id, EventAction::Restarting, None);
        }
    });
    if let Some(delay) = restart_in {
        spawn_restarter(inner.clone(), id.to_owned(), delay);
    }
}

/// A run at least this long resets the restart backoff.
const STABLE_RUN: Duration = Duration::from_secs(10);

/// Starts the machine again after `delay`, unless it was stopped or removed
/// meanwhile.
fn spawn_restarter(inner: Arc<Inner>, id: String, delay: Duration) {
    std::thread::spawn(move || {
        let deadline = Instant::now() + delay;
        let mut guard = inner.lock();
        loop {
            match inner.store.load_state(&id) {
                Ok(s) if s.status == Status::Restarting => {}
                _ => return,
            }
            match deadline.checked_duration_since(Instant::now()) {
                Some(left) if !left.is_zero() => {
                    guard = inner
                        .changed
                        .wait_timeout(guard, left)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                _ => break,
            }
        }
        drop(guard);
        if let Err(err) = Machine::new(inner.clone(), id.clone()).start_with(true)
            && err.kind() != ErrorKind::NotFound
        {
            // Could not boot again: the machine stays down, with the reason.
            let _ = inner.update_state(&id, |s| {
                if s.status == Status::Restarting {
                    s.status = Status::Exited;
                    s.error = err.message().to_owned();
                }
            });
        }
    });
}

/// Starts the host side of the machine's tunnels and its configured bindings.
/// `fresh` is false on reattach: the guest still holds its listeners.
fn start_tunnels(inner: &Inner, id: &str, host: &HostConfig, fresh: bool) -> Result<()> {
    let tunnels = MachineTunnels::start(inner.socket_path(id), inner.host_socket_path(id))?;
    tunnels.publish_configured(host)?;
    if fresh {
        tunnels.listen_configured(host)?;
    }
    inner.tunnels().insert(id.to_owned(), tunnels);
    Ok(())
}

/// Picks up a machine recorded as running by a previous process.
pub(crate) fn reattach(inner: &Arc<Inner>, id: &str) {
    let Ok(state) = inner.store.load_state(id) else {
        return;
    };
    if state.status == Status::Restarting {
        // The library went away during a restart delay: carry on.
        spawn_restarter(inner.clone(), id.to_owned(), inner.restart_delay);
        return;
    }
    if state.status != Status::Running {
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    if let Ok(mut stream) = EventStream::connect(&inner.socket_path(id))
        && let Next::Event(Event::Ready) = stream.next(Some(deadline))
    {
        if let Ok((record, _)) = inner.store.load(id) {
            // Best effort: ports that cannot be re-published are simply absent.
            let _ = start_tunnels(inner, id, &record.host_config, false);
        }
        spawn_monitor(inner.clone(), id.to_owned(), Ready::Stream(stream));
        return;
    }
    let code = read_exit_file(&inner.store.machine_dir(id).join("exit"));
    record_exit(inner, id, code);
}

/// Forcefully ends the VMM helper and everything in its process group.
fn kill_group(pid: u32) {
    // SAFETY: plain signal delivery; a stale pid at worst fails with ESRCH.
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
        libc::kill(pid as i32, libc::SIGKILL);
    }
}
