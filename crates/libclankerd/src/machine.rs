//! `Machine`: a handle on one machine, plus the start/monitor lifecycle.

use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use clankerd_proto::guest::{Event, Workload};
use serde::Serialize;

use crate::config::{HostConfig, MachineConfig};
use crate::engine::Inner;
use crate::error::{Error, Result};
use crate::exec::{Exec, ExecConfig};
use crate::guest::{EventStream, Next};
use crate::state::{MachineState, Status, WaitResult};
use crate::vmm::BootSpec;

/// Everything `inspect` reports: the creation-time record plus current state.
#[derive(Debug, Clone, Serialize)]
pub struct MachineInfo {
    pub id: String,
    pub name: String,
    pub created: SystemTime,
    pub config: MachineConfig,
    pub host_config: HostConfig,
    pub state: MachineState,
}

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
            exit_file: exit_file.clone(),
            workload: Workload {
                argv,
                env: info.config.env.clone(),
                working_dir: info.config.working_dir.clone(),
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
        let ready = match wait_ready(&socket, &exit_file, self.inner.start_timeout) {
            Ok(r) => r,
            Err(err) => {
                kill_group(handle.pid);
                let _ = fs::remove_file(&socket);
                return Err(fail(err));
            }
        };

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

    /// Blocks until the machine is not running and returns its exit code.
    /// Returns immediately for a machine that never started.
    pub fn wait(&self) -> Result<WaitResult> {
        let mut starting = self.inner.lock();
        loop {
            let state = self.inner.store.load_state(&self.id)?;
            if state.status != Status::Running && !starting.contains(&self.id) {
                return Ok(WaitResult {
                    exit_code: state.exit_code,
                });
            }
            starting = self
                .inner
                .changed
                .wait_timeout(starting, Duration::from_millis(500))
                .unwrap_or_else(|e| e.into_inner())
                .0;
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

fn wait_ready(socket: &Path, exit_file: &Path, timeout: Duration) -> Result<Ready> {
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
    let _ = fs::remove_file(inner.socket_path(id));
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
