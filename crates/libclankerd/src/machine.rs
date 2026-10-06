//! `Machine`: a handle on one machine, plus the start/monitor lifecycle.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use clankerd_proto::guest::{Event, METHOD_KILL, METHOD_SHUTDOWN, SignalParams, Workload};
use serde::Serialize;

use crate::config::{HostConfig, MachineConfig, PortBinding};
use crate::engine::Inner;
use crate::error::{Error, ErrorKind, Result};
use crate::exec::{Exec, ExecConfig};
use crate::guest::{self, EventStream, Next};
use crate::image_config::ImageConfig;
use crate::state::{MachineState, Status, WaitResult};
use crate::tunnel::{GuestBinding, MachineTunnels, PublishedPort, validate_port_binding};
use crate::vmm::BootSpec;

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
        let info = self.inspect()?;
        {
            let mut starting = self.inner.lock();
            let status = self.inner.store.load_state(&self.id)?.status;
            if status == Status::Running || starting.contains(&self.id) {
                return Err(Error::conflict(format!(
                    "machine {} is already running",
                    info.name
                )));
            }
            starting.insert(self.id.clone());
        }
        let result = self.boot(&info);
        self.inner.lock().remove(&self.id);
        self.inner.changed.notify_all();
        result
    }

    fn boot(&self, info: &MachineInfo) -> Result<()> {
        let dir = self.inner.store.machine_dir(&self.id);
        let exit_file = dir.join("exit");
        let socket = self.inner.socket_path(&self.id);
        let _ = fs::remove_file(&exit_file);

        let mut argv = info.config.entrypoint.clone();
        argv.extend(info.config.cmd.iter().cloned());
        let spec = BootSpec {
            machine_id: self.id.clone(),
            dir,
            guest_socket: socket.clone(),
            host_socket: self.inner.host_socket_path(&self.id),
            exit_file: exit_file.clone(),
            workload: Workload {
                argv,
                env: info.config.env.clone(),
                working_dir: info.config.working_dir.clone(),
                user: info.config.user.clone(),
            },
            cpus: info.host_config.cpus,
            memory: info.host_config.memory,
        };

        let fail = |err: Error| {
            let _ = self
                .inner
                .update_state(&self.id, |s| s.error = err.message().to_owned());
            err
        };
        let handle = self.inner.vmm.boot(&spec).map_err(fail)?;
        let alive = || self.inner.vmm.check_alive(&spec, &handle);
        let ready = match wait_ready(&socket, &exit_file, self.inner.start_timeout, &alive) {
            Ok(r) => r,
            Err(err) => {
                kill_group(handle.pid);
                let _ = fs::remove_file(&socket);
                return Err(fail(err));
            }
        };

        if matches!(ready, Ready::Stream(_))
            && let Err(err) = start_tunnels(&self.inner, &self.id, &info.host_config, true)
        {
            kill_group(handle.pid);
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
        })?;
        spawn_monitor(self.inner.clone(), self.id.clone(), ready);
        Ok(())
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

    fn tunnels(&self) -> Result<Arc<MachineTunnels>> {
        self.inner
            .tunnels()
            .get(&self.id)
            .cloned()
            .ok_or_else(|| Error::conflict("machine is not running"))
    }

    /// Publishes a guest port on host loopback while the machine runs.
    /// Dropping the returned handle closes the listener.
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
            .wait_until(None)?
            .expect("waiting without a deadline ends with a result"))
    }

    /// Like [`Machine::wait`], giving up with `None` at `deadline`.
    fn wait_until(&self, deadline: Option<Instant>) -> Result<Option<WaitResult>> {
        let mut starting = self.inner.lock();
        loop {
            let state = self.inner.store.load_state(&self.id)?;
            if state.status != Status::Running && !starting.contains(&self.id) {
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

    /// Stops the machine gracefully: guestd signals the workload (SIGTERM),
    /// and once it exits stops the remaining processes, syncs, unmounts and
    /// powers off. If the machine is still running after `timeout` (or guestd
    /// cannot be reached) the VMM is killed. Stopping a machine that is not
    /// running succeeds.
    pub fn stop(&self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        if self.inner.store.load_state(&self.id)?.status != Status::Running {
            return Ok(());
        }
        let asked = guest::call(
            &self.inner.socket_path(&self.id),
            METHOD_SHUTDOWN,
            &serde_json::json!({}),
        );
        if asked.is_ok() && self.wait_until(Some(deadline))?.is_some() {
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
        self.inner.forced().insert(self.id.clone());
        kill_group(pid);
        // The monitor records the end; return once it has.
        self.wait_until(Some(Instant::now() + Duration::from_secs(10)))?;
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

    /// Deletes the machine and its state. A running machine needs `force`.
    pub fn remove(&self, force: bool) -> Result<()> {
        let mut starting = self.inner.lock();
        let (record, state) = self.inner.store.load(&self.id)?;
        if starting.contains(&self.id) {
            return Err(Error::conflict(format!(
                "machine {} is starting",
                record.name
            )));
        }
        if state.status == Status::Running {
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
        self.inner.drop_tunnels(&self.id);
        self.inner.store.remove(&self.id)?;
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
fn record_exit(inner: &Inner, id: &str, code: Option<i32>) {
    // Release host resources first: whoever sees the new state sees them gone.
    inner.drop_tunnels(id);
    let _ = fs::remove_file(inner.socket_path(id));
    // A machine whose helper we killed ended by SIGKILL: Docker's 137.
    let code = code.or_else(|| inner.forced().remove(id).then_some(137));
    let _ = inner.update_state(id, |s| {
        if s.status != Status::Running {
            return;
        }
        s.status = if code.is_some() {
            Status::Exited
        } else {
            Status::Dead
        };
        s.exit_code = code.unwrap_or(-1);
        s.pid = None;
        s.finished_at = Some(SystemTime::now());
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
