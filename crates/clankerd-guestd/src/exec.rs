//! Exec sessions: processes started on behalf of the host, with piped stdio or a PTY.
//!
//! Lifecycle (Docker naming): `ExecCreate` registers a spec, `ExecStart`
//! (varlink `upgrade`) runs it and turns the connection into a framed stream,
//! `ExecResize` / `ExecKill` / `ExecInspect` act on a session from other connections.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clankerd_proto::frame::{self, Frame};
use clankerd_proto::guest::{
    ERROR_CONFLICT, ERROR_INVALID_PARAMETER, ERROR_NO_SUCH_EXEC, ExecCreated, ExecRef, ExecSpec,
    ExecState, ExecStatus, ResizeParams, SignalParams,
};

use crate::user;

const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";
/// How long to wait for output pipes to reach EOF after the process exited
/// (a background grandchild may keep them open).
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// A varlink-level failure: error name plus message.
pub type Failure = (&'static str, String);

pub struct Registry {
    base_env: Vec<String>,
    base_cwd: String,
    base_user: String,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
}

struct Session {
    spec: ExecSpec,
    state: Mutex<SessionState>,
}

#[derive(Default)]
struct SessionState {
    started: bool,
    pid: Option<i32>,
    exit: Option<ExecStatus>,
    master: Option<Arc<OwnedFd>>,
}

impl Registry {
    pub fn new(base_env: Vec<String>, base_cwd: String, base_user: String) -> Self {
        Registry {
            base_env,
            base_cwd,
            base_user,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn get(&self, id: &str) -> Result<Arc<Session>, Failure> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or((ERROR_NO_SUCH_EXEC, format!("no such exec {id}")))
    }

    pub fn create(&self, spec: ExecSpec) -> Result<ExecCreated, Failure> {
        if spec.argv.is_empty() {
            return Err((ERROR_INVALID_PARAMETER, "exec needs a command".into()));
        }
        let id = new_id();
        self.sessions.lock().unwrap().insert(
            id.clone(),
            Arc::new(Session {
                spec,
                state: Mutex::default(),
            }),
        );
        Ok(ExecCreated { id })
    }

    pub fn resize(&self, p: &ResizeParams) -> Result<(), Failure> {
        self.get(&p.id)?.resize(p.rows, p.cols)
    }

    pub fn kill(&self, p: &SignalParams) -> Result<(), Failure> {
        self.get(&p.id)?.kill(p.signal)
    }

    pub fn inspect(&self, r: &ExecRef) -> Result<ExecState, Failure> {
        let s = self.get(&r.id)?;
        let st = s.state.lock().unwrap();
        Ok(ExecState {
            running: st.pid.is_some() && st.exit.is_none(),
            exit_code: st.exit.as_ref().map(|e| e.exit_code),
            signal: st.exit.as_ref().and_then(|e| e.signal),
            pid: st.pid,
        })
    }

    /// Claims a created session for `ExecStart`.
    pub fn claim(&self, id: &str) -> Result<ExecHandle, Failure> {
        let s = self.get(id)?;
        let mut st = s.state.lock().unwrap();
        if st.started {
            return Err((ERROR_CONFLICT, format!("exec {id} was already started")));
        }
        st.started = true;
        drop(st);
        Ok(ExecHandle {
            session: s,
            base_env: self.base_env.clone(),
            base_cwd: self.base_cwd.clone(),
            base_user: self.base_user.clone(),
        })
    }
}

fn new_id() -> String {
    let mut b = [0u8; 16];
    if File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_err()
    {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        b = n.to_le_bytes();
    }
    b.iter().map(|x| format!("{x:02x}")).collect()
}

impl Session {
    fn resize(&self, rows: u16, cols: u16) -> Result<(), Failure> {
        let st = self.state.lock().unwrap();
        let Some(master) = &st.master else {
            return Err((ERROR_CONFLICT, "exec has no terminal".into()));
        };
        set_winsize(master.as_raw_fd(), rows, cols);
        Ok(())
    }

    /// Signals the process group (the exec is its own session leader).
    fn kill(&self, signal: i32) -> Result<(), Failure> {
        let st = self.state.lock().unwrap();
        match (st.pid, &st.exit) {
            (Some(pid), None) => {
                // SAFETY: plain signal delivery to a process we spawned and have not reaped.
                unsafe { libc::kill(-pid, signal) };
                Ok(())
            }
            _ => Err((ERROR_CONFLICT, "exec is not running".into())),
        }
    }
}

fn set_winsize(fd: i32, rows: u16, cols: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: TIOCSWINSZ reads a winsize from a valid pointer.
    unsafe { libc::ioctl(fd, libc::TIOCSWINSZ, &ws) };
}

/// Serialises frame writes and drops anything sent after the stream was closed,
/// which keeps the status frame last.
struct Sink(Mutex<Option<UnixStream>>);

impl Sink {
    fn send(&self, frame: &Frame) {
        let mut g = self.0.lock().unwrap();
        if let Some(conn) = g.as_mut()
            && frame::write_frame(conn, frame).is_err()
        {
            *g = None;
        }
    }

    fn output(&self, channel: u8, data: &[u8]) {
        for chunk in data.chunks(frame::MAX_PAYLOAD) {
            self.send(&Frame::new(channel, chunk));
        }
    }

    fn finish(&self, status: &ExecStatus) {
        self.send(&Frame::new(
            frame::CHANNEL_STATUS,
            serde_json::to_vec(status).unwrap(),
        ));
        if let Some(conn) = self.0.lock().unwrap().take() {
            let _ = conn.shutdown(std::net::Shutdown::Both);
        }
    }
}

pub struct ExecHandle {
    session: Arc<Session>,
    base_env: Vec<String>,
    base_cwd: String,
    base_user: String,
}

struct Spawned {
    child: std::process::Child,
    stdin: Option<File>,
    outputs: Vec<(u8, File)>,
}

impl ExecHandle {
    /// Runs the session to completion on an upgraded connection.
    pub fn run(self, input: BufReader<UnixStream>, out: UnixStream) {
        let sink = Arc::new(Sink(Mutex::new(Some(out))));
        let spawned = match self.spawn() {
            Ok(s) => s,
            Err((code, msg)) => {
                sink.output(frame::CHANNEL_STDERR, format!("{msg}\n").as_bytes());
                let status = ExecStatus {
                    exit_code: code,
                    signal: None,
                };
                self.session.state.lock().unwrap().exit = Some(status.clone());
                sink.finish(&status);
                return;
            }
        };
        let Spawned {
            mut child,
            stdin,
            outputs,
        } = spawned;
        self.session.state.lock().unwrap().pid = Some(child.id() as i32);

        let (done_tx, done_rx) = mpsc::channel::<()>();
        let readers = outputs.len();
        for (channel, mut file) in outputs {
            let (sink, done) = (sink.clone(), done_tx.clone());
            std::thread::spawn(move || {
                let mut buf = vec![0u8; 32 * 1024];
                // EIO is how a PTY master reports that the slave side is gone.
                while let Ok(n) = file.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    sink.output(channel, &buf[..n]);
                }
                let _ = done.send(());
            });
        }
        drop(done_tx);

        let stdin_tx = stdin.map(|f| spawn_stdin_writer(f, self.session.spec.tty));
        {
            let session = self.session.clone();
            std::thread::spawn(move || pump_input(input, session, stdin_tx));
        }

        let status = match child.wait() {
            Ok(s) => ExecStatus {
                exit_code: s.code().unwrap_or_else(|| 128 + s.signal().unwrap_or(0)),
                signal: s.signal(),
            },
            Err(_) => ExecStatus {
                exit_code: 255,
                signal: None,
            },
        };
        // Everything the process wrote must reach the host before its status.
        let deadline = Instant::now() + DRAIN_GRACE;
        for _ in 0..readers {
            let left = deadline.saturating_duration_since(Instant::now());
            if done_rx.recv_timeout(left).is_err() {
                break;
            }
        }
        {
            let mut st = self.session.state.lock().unwrap();
            st.exit = Some(status.clone());
            st.master = None;
        }
        sink.finish(&status);
    }

    fn spawn(&self) -> Result<Spawned, (i32, String)> {
        let spec = &self.session.spec;
        // Like Docker, an exec runs as the machine's user unless told otherwise.
        let user_spec = if spec.user.is_empty() {
            &self.base_user
        } else {
            &spec.user
        };
        let resolved = user::lookup(user_spec).map_err(|e| (126, e))?;

        let mut cmd = Command::new(&spec.argv[0]);
        cmd.args(&spec.argv[1..]).env_clear();
        cmd.env("PATH", DEFAULT_PATH);
        if let Some(home) = resolved.as_ref().and_then(|r| r.home.as_ref()) {
            cmd.env("HOME", home);
        }
        if spec.tty {
            cmd.env("TERM", "xterm");
        }
        for kv in self.base_env.iter().chain(&spec.env) {
            if let Some((k, v)) = kv.split_once('=') {
                cmd.env(k, v);
            }
        }
        let cwd = if spec.working_dir.is_empty() {
            &self.base_cwd
        } else {
            &spec.working_dir
        };
        if !cwd.is_empty() {
            cmd.current_dir(cwd);
        }

        let tty = spec.tty;
        // SAFETY: only async-signal-safe libc calls between fork and exec.
        unsafe {
            cmd.pre_exec(move || {
                if libc::setsid() < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if tty && libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        if let Some(user) = &resolved {
            user::drop_privileges(&mut cmd, user);
        }

        let fail = |e: std::io::Error| {
            let code = if e.kind() == std::io::ErrorKind::NotFound {
                127
            } else {
                126
            };
            (code, format!("cannot run {}: {e}", spec.argv[0]))
        };

        if spec.tty {
            let (master, slave) = open_pty(spec.size.as_ref().map(|s| (s.rows, s.cols)))
                .map_err(|e| (126, format!("opening pty: {e}")))?;
            let dup = |fd: &OwnedFd| fd.try_clone().map_err(|e| (126, e.to_string()));
            cmd.stdin(Stdio::from(dup(&slave)?))
                .stdout(Stdio::from(dup(&slave)?))
                .stderr(Stdio::from(slave));
            let child = cmd.spawn().map_err(fail)?;
            // The Command still owns slave dups; release them so EOF can happen.
            drop(cmd);
            let master = Arc::new(master);
            self.session.state.lock().unwrap().master = Some(master.clone());
            let read = File::from(master.try_clone().map_err(|e| (126, e.to_string()))?);
            let write = File::from(master.try_clone().map_err(|e| (126, e.to_string()))?);
            Ok(Spawned {
                child,
                stdin: Some(write),
                outputs: vec![(frame::CHANNEL_STDOUT, read)],
            })
        } else {
            cmd.stdin(if spec.attach_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
            let mut child = cmd.spawn().map_err(fail)?;
            let stdin = child.stdin.take().map(|p| File::from(OwnedFd::from(p)));
            let out = File::from(OwnedFd::from(child.stdout.take().unwrap()));
            let err = File::from(OwnedFd::from(child.stderr.take().unwrap()));
            Ok(Spawned {
                child,
                stdin,
                outputs: vec![(frame::CHANNEL_STDOUT, out), (frame::CHANNEL_STDERR, err)],
            })
        }
    }
}

fn open_pty(size: Option<(u16, u16)>) -> std::io::Result<(OwnedFd, OwnedFd)> {
    let (mut m, mut s) = (-1, -1);
    // SAFETY: openpty fills two fds we then own; null name/termios/winsize are allowed.
    let rc = unsafe {
        libc::openpty(
            &mut m,
            &mut s,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if rc < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: both fds are fresh and owned by nobody else.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
    for fd in [&master, &slave] {
        // SAFETY: setting close-on-exec on a valid fd; Command dups the slave onto 0..2.
        unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) };
    }
    if let Some((rows, cols)) = size {
        set_winsize(master.as_raw_fd(), rows, cols);
    }
    Ok((master, slave))
}

/// Writes queued stdin data on its own thread so a process that does not read
/// cannot stall resize and signal handling. Dropping the sender is EOF.
fn spawn_stdin_writer(mut file: File, tty: bool) -> Sender<Vec<u8>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        for data in rx {
            if file.write_all(&data).is_err() {
                return;
            }
        }
        if tty {
            // A terminal has no half-close: send the end-of-file character.
            let _ = file.write_all(&[0x04]);
        }
    });
    tx
}

/// Handles frames from the host until the connection ends.
fn pump_input(
    mut input: BufReader<UnixStream>,
    session: Arc<Session>,
    mut stdin: Option<Sender<Vec<u8>>>,
) {
    loop {
        match frame::read_frame(&mut input) {
            Ok(Some(f)) => match f.channel {
                frame::CHANNEL_STDIN => {
                    if let Some(tx) = &stdin {
                        let _ = tx.send(f.payload);
                    }
                }
                frame::CHANNEL_CLOSE if f.payload == [frame::CHANNEL_STDIN] => stdin = None,
                frame::CHANNEL_RESIZE => {
                    if let Ok(r) = serde_json::from_slice::<ResizeParams>(&f.payload) {
                        let _ = session.resize(r.rows, r.cols);
                    }
                }
                frame::CHANNEL_SIGNAL => {
                    if let Ok(s) = serde_json::from_slice::<SignalParams>(&f.payload) {
                        let _ = session.kill(s.signal);
                    }
                }
                _ => {}
            },
            // The host went away: hang the process up rather than leak it.
            Ok(None) | Err(_) => {
                let _ = session.kill(libc::SIGHUP);
                return;
            }
        }
    }
}
