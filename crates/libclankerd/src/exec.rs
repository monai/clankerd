//! `Exec`: a command run inside a running machine (Docker's exec lifecycle).
//!
//! `Machine::exec_create` registers the command in the guest, `Exec::start`
//! upgrades a connection to a framed stream and returns [`ExecStreams`];
//! `resize`, `kill` and `inspect` act on the session over their own connections.

use std::io::{self, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use clankerd_proto::frame::{self, Frame};
use clankerd_proto::guest::{
    ExecCreated, ExecRef, ExecSpec, METHOD_EXEC_CREATE, METHOD_EXEC_INSPECT, METHOD_EXEC_KILL,
    METHOD_EXEC_RESIZE, METHOD_EXEC_START, ResizeParams, SignalParams,
};

use crate::error::{Error, Result};
use crate::guest;

/// Snapshot returned by [`Exec::inspect`].
pub use clankerd_proto::guest::ExecState as ExecInfo;
/// Exit status of an exec; `signal` is set when the process died from one
/// (`exit_code` is then 128 + signal, as in Docker).
pub use clankerd_proto::guest::ExecStatus;

/// What to run (Docker's `ExecCreate` body).
#[derive(Debug, Clone, Default)]
pub struct ExecConfig {
    /// Program and arguments.
    pub cmd: Vec<String>,
    /// `KEY=value`, layered over the machine's environment.
    pub env: Vec<String>,
    /// Empty: the machine's working directory.
    pub working_dir: String,
    /// `user[:group]`, resolved against the guest's passwd and group files.
    pub user: String,
    /// Allocate a PTY (stdout and stderr are then one stream).
    pub tty: bool,
    pub attach_stdin: bool,
    /// Initial terminal size `(rows, cols)` for a tty.
    pub size: Option<(u16, u16)>,
}

/// A created exec. Cheap to clone.
#[derive(Debug, Clone)]
pub struct Exec {
    socket: PathBuf,
    id: String,
    attach_stdin: bool,
}

impl Exec {
    pub(crate) fn create(socket: PathBuf, config: ExecConfig) -> Result<Exec> {
        if config.cmd.is_empty() {
            return Err(Error::invalid_parameter("exec needs a command"));
        }
        let spec = ExecSpec {
            argv: config.cmd,
            env: config.env,
            working_dir: config.working_dir,
            user: config.user,
            tty: config.tty,
            attach_stdin: config.attach_stdin,
            size: config.size.map(|(rows, cols)| ResizeParams {
                id: String::new(),
                rows,
                cols,
            }),
        };
        let created: ExecCreated =
            serde_json::from_value(guest::call(&socket, METHOD_EXEC_CREATE, &spec)?)?;
        Ok(Exec {
            socket,
            id: created.id,
            attach_stdin: config.attach_stdin,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Runs the command and returns its stdio streams. An exec starts once.
    pub fn start(&self) -> Result<ExecStreams> {
        let reader = guest::upgrade(
            &self.socket,
            METHOD_EXEC_START,
            &ExecRef {
                id: self.id.clone(),
            },
        )?;
        let writer = reader.get_ref().try_clone()?;
        let (out_tx, out_rx) = mpsc::channel();
        let (err_tx, err_rx) = mpsc::channel();
        let (status_tx, status_rx) = mpsc::channel();
        std::thread::spawn(move || demux(reader, out_tx, err_tx, status_tx));
        let writer = Arc::new(Mutex::new(writer));
        Ok(ExecStreams {
            stdin: self.attach_stdin.then(|| ExecStdin {
                writer: writer.clone(),
            }),
            stdout: ExecOutput::new(out_rx),
            stderr: ExecOutput::new(err_rx),
            status_rx,
            status: None,
            _writer: writer,
        })
    }

    /// Sets the terminal size of a tty exec (TIOCSWINSZ in the guest).
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        let p = ResizeParams {
            id: self.id.clone(),
            rows,
            cols,
        };
        guest::call(&self.socket, METHOD_EXEC_RESIZE, &p).map(drop)
    }

    /// Sends `signal` (a Linux signal number) to the exec's process group.
    pub fn kill(&self, signal: i32) -> Result<()> {
        let p = SignalParams {
            id: self.id.clone(),
            signal,
        };
        guest::call(&self.socket, METHOD_EXEC_KILL, &p).map(drop)
    }

    pub fn inspect(&self) -> Result<ExecInfo> {
        let v = guest::call(
            &self.socket,
            METHOD_EXEC_INSPECT,
            &ExecRef {
                id: self.id.clone(),
            },
        )?;
        Ok(serde_json::from_value(v)?)
    }
}

fn demux(
    mut r: BufReader<UnixStream>,
    out: Sender<Vec<u8>>,
    err: Sender<Vec<u8>>,
    status: Sender<ExecStatus>,
) {
    while let Ok(Some(f)) = frame::read_frame(&mut r) {
        match f.channel {
            frame::CHANNEL_STDOUT => {
                let _ = out.send(f.payload);
            }
            frame::CHANNEL_STDERR => {
                let _ = err.send(f.payload);
            }
            frame::CHANNEL_STATUS => {
                if let Ok(s) = serde_json::from_slice(&f.payload) {
                    let _ = status.send(s);
                }
                return;
            }
            _ => {}
        }
    }
}

/// The stdio of a started exec. Output stays readable after [`ExecStreams::wait`].
pub struct ExecStreams {
    /// Present when the exec was created with `attach_stdin`.
    pub stdin: Option<ExecStdin>,
    pub stdout: ExecOutput,
    /// Always at end of file for a tty exec.
    pub stderr: ExecOutput,
    status_rx: Receiver<ExecStatus>,
    status: Option<ExecStatus>,
    /// Keeps the connection open while the streams live.
    _writer: Arc<Mutex<UnixStream>>,
}

impl ExecStreams {
    /// Blocks until the process ended. All its output has been delivered to
    /// `stdout` and `stderr` by then.
    pub fn wait(&mut self) -> Result<ExecStatus> {
        if self.status.is_none() {
            self.status = Some(
                self.status_rx
                    .recv()
                    .map_err(|_| Error::unavailable("lost the guest before the exec finished"))?,
            );
        }
        Ok(self.status.clone().unwrap())
    }
}

/// Write side of the exec's stdin. Dropping it, or [`ExecStdin::close`], is EOF.
pub struct ExecStdin {
    writer: Arc<Mutex<UnixStream>>,
}

impl ExecStdin {
    /// Signals end of input.
    pub fn close(self) {}
}

impl Write for ExecStdin {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(frame::MAX_PAYLOAD);
        let mut w = self.writer.lock().unwrap();
        frame::write_frame(&mut *w, &Frame::new(frame::CHANNEL_STDIN, &buf[..n]))?;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for ExecStdin {
    fn drop(&mut self) {
        let mut w = self.writer.lock().unwrap();
        let _ = frame::write_frame(&mut *w, &Frame::close(frame::CHANNEL_STDIN));
    }
}

/// Read side of stdout or stderr; end of file once the exec's output is done.
pub struct ExecOutput {
    rx: Receiver<Vec<u8>>,
    pending: Vec<u8>,
    pos: usize,
}

impl ExecOutput {
    fn new(rx: Receiver<Vec<u8>>) -> Self {
        ExecOutput {
            rx,
            pending: Vec::new(),
            pos: 0,
        }
    }
}

impl ExecOutput {
    /// An output that is already at end of file.
    pub fn empty() -> Self {
        ExecOutput::new(mpsc::channel().1)
    }
}

impl Read for ExecOutput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.pending.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.pending = chunk;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}
