//! Attach: the host connects to the workload's terminal or stdio (Docker's
//! `attach`, behind `run -it`).
//!
//! With `tty` or `open_stdin` the workload is spawned on a PTY or pipes that
//! this module owns. Its output always goes to the console as well (so
//! `vmctl logs` shows it) and, while somebody is attached, to that connection;
//! what was written before the first attach is kept (bounded) and replayed.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clankerd_proto::frame;
use clankerd_proto::guest::{AttachParams, ERROR_CONFLICT, ExecStatus, Workload};
use serde_json::Value;

use crate::exec::{Failure, Sink, open_pty, pump_input, set_winsize, spawn_stdin_writer};

/// Writes a varlink reply on the connection (main's `reply`).
pub type Replier = dyn Fn(&mut UnixStream, Result<Value, Failure>) -> std::io::Result<()>;

/// Output kept for replay while nobody is attached.
const BACKLOG_LIMIT: usize = 256 * 1024;
/// How long to wait for output pipes to reach EOF after the workload exited.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

#[derive(Default)]
pub struct Attach {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    enabled: bool,
    master: Option<Arc<OwnedFd>>,
    sink: Option<Arc<Sink>>,
    backlog: VecDeque<(u8, Vec<u8>)>,
    backlog_len: usize,
    status: Option<ExecStatus>,
    stdin: Option<Sender<Vec<u8>>>,
    readers: usize,
    done: Option<Receiver<()>>,
}

impl Attach {
    pub fn wanted(w: &Workload) -> bool {
        w.tty || w.open_stdin
    }

    /// Spawns the workload on a PTY or pipes. Call instead of a plain spawn
    /// when [`Attach::wanted`].
    pub fn spawn(self: &Arc<Self>, cmd: &mut Command, w: &Workload) -> std::io::Result<Child> {
        if w.tty {
            // SAFETY: only async-signal-safe libc calls between fork and exec.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    if libc::ioctl(0, libc::TIOCSCTTY as _, 0) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let (stdin, outputs, child);
        if w.tty {
            let (master, slave) = open_pty(None)?;
            let dup = |fd: &OwnedFd| fd.try_clone();
            cmd.stdin(Stdio::from(dup(&slave)?))
                .stdout(Stdio::from(dup(&slave)?))
                .stderr(Stdio::from(slave));
            child = crate::power::spawn_owned(cmd)?;
            let master = Arc::new(master);
            self.state.lock().unwrap().master = Some(master.clone());
            stdin = Some(File::from(master.try_clone()?));
            outputs = vec![(frame::CHANNEL_STDOUT, File::from(master.try_clone()?))];
        } else {
            cmd.stdin(if w.open_stdin {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
            let mut c = crate::power::spawn_owned(cmd)?;
            stdin = c.stdin.take().map(|p| File::from(OwnedFd::from(p)));
            outputs = vec![
                (
                    frame::CHANNEL_STDOUT,
                    File::from(OwnedFd::from(c.stdout.take().unwrap())),
                ),
                (
                    frame::CHANNEL_STDERR,
                    File::from(OwnedFd::from(c.stderr.take().unwrap())),
                ),
            ];
            child = c;
        }
        let (done_tx, done_rx) = mpsc::channel();
        {
            let mut st = self.state.lock().unwrap();
            st.enabled = true;
            st.readers = outputs.len();
            st.done = Some(done_rx);
            st.stdin = stdin.map(|f| spawn_stdin_writer(f, w.tty));
        }
        for (channel, file) in outputs {
            let (this, done) = (self.clone(), done_tx.clone());
            std::thread::spawn(move || {
                this.read_output(channel, file);
                let _ = done.send(());
            });
        }
        Ok(child)
    }

    fn read_output(&self, channel: u8, mut file: File) {
        let mut buf = vec![0u8; 32 * 1024];
        // EIO is how a PTY master reports that the slave side is gone.
        while let Ok(n) = file.read(&mut buf) {
            if n == 0 {
                break;
            }
            let data = &buf[..n];
            if channel == frame::CHANNEL_STDERR {
                let _ = std::io::stderr().write_all(data);
            } else {
                let mut out = std::io::stdout().lock();
                let _ = out.write_all(data);
                let _ = out.flush();
            }
            let mut st = self.state.lock().unwrap();
            match st.sink.clone() {
                Some(sink) => sink.output(channel, data),
                None => {
                    st.backlog_len += data.len();
                    st.backlog.push_back((channel, data.to_vec()));
                    while st.backlog_len > BACKLOG_LIMIT {
                        match st.backlog.pop_front() {
                            Some((_, old)) => st.backlog_len -= old.len(),
                            None => break,
                        }
                    }
                }
            }
        }
    }

    /// Records the workload's end: once its output was delivered, the attached
    /// connection (if any) gets the status and is closed.
    pub fn finish(&self, status: ExecStatus) {
        let (done, readers) = {
            let mut st = self.state.lock().unwrap();
            if !st.enabled {
                return;
            }
            (st.done.take(), st.readers)
        };
        if let Some(done) = done {
            let deadline = Instant::now() + DRAIN_GRACE;
            for _ in 0..readers {
                let left = deadline.saturating_duration_since(Instant::now());
                if done.recv_timeout(left).is_err() {
                    break;
                }
            }
        }
        let sink = {
            let mut st = self.state.lock().unwrap();
            st.status = Some(status.clone());
            st.sink.clone()
        };
        if let Some(sink) = sink {
            sink.finish(&status);
        }
    }

    /// Serves one attached connection until the host detaches or the workload
    /// ends. `signal` delivers signals sent by the host to the workload.
    pub fn run(
        &self,
        params: AttachParams,
        input: BufReader<UnixStream>,
        out: UnixStream,
        signal: impl Fn(i32),
        reply: &Replier,
    ) {
        let mut conn = match out.try_clone() {
            Ok(c) => c,
            Err(_) => return,
        };
        let sink = Arc::new(Sink::new(out));
        let stdin = {
            let mut st = self.state.lock().unwrap();
            let refusal = if !st.enabled {
                Some("the workload has neither a terminal nor an open stdin")
            } else if st.sink.is_some() {
                Some("the workload is already attached")
            } else {
                None
            };
            if let Some(why) = refusal {
                let _ = reply(&mut conn, Err((ERROR_CONFLICT, why.into())));
                return;
            }
            if reply(&mut conn, Ok(serde_json::json!({}))).is_err() {
                return;
            }
            if let (Some(size), Some(master)) = (&params.size, &st.master) {
                set_winsize(master.as_raw_fd(), size.rows, size.cols);
            }
            for (channel, data) in std::mem::take(&mut st.backlog) {
                sink.output(channel, &data);
            }
            st.backlog_len = 0;
            if let Some(status) = st.status.clone() {
                sink.finish(&status);
                return;
            }
            st.sink = Some(sink.clone());
            st.stdin.take()
        };
        let master = self.state.lock().unwrap().master.clone();
        let stdin = pump_input(
            input,
            |rows, cols| {
                if let Some(master) = &master {
                    set_winsize(master.as_raw_fd(), rows, cols);
                }
            },
            signal,
            stdin,
        );
        let mut st = self.state.lock().unwrap();
        st.sink = None;
        st.stdin = stdin;
    }
}
