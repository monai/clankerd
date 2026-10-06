//! `vmctl exec [-it]`: run a command in a running machine.

use std::io::{self, Read, Write};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use clap::Args;
use libclankerd::{Engine, Error, ExecConfig};

#[derive(Args)]
pub struct ExecArgs {
    /// Keep stdin open and forward it.
    #[arg(short = 'i', long)]
    interactive: bool,
    /// Allocate a pseudo-terminal.
    #[arg(short = 't', long)]
    tty: bool,
    #[arg(short = 'u', long)]
    user: Option<String>,
    #[arg(short = 'w', long)]
    workdir: Option<String>,
    /// Environment variable, KEY=value.
    #[arg(short = 'e', long = "env")]
    env: Vec<String>,
    machine: String,
    /// Command and arguments.
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true)]
    cmd: Vec<String>,
}

pub fn run(engine: &Engine, args: ExecArgs) -> Result<u8, Error> {
    let stdin_is_tty = isatty(libc::STDIN_FILENO);
    if args.tty && !stdin_is_tty {
        return Err(Error::invalid_parameter("the input device is not a TTY"));
    }
    let machine = engine.get(&args.machine)?;
    let exec = machine.exec_create(ExecConfig {
        cmd: args.cmd,
        env: args.env,
        working_dir: args.workdir.unwrap_or_default(),
        user: args.user.unwrap_or_default(),
        tty: args.tty,
        attach_stdin: args.interactive || args.tty,
        size: if args.tty { window_size() } else { None },
    })?;
    let mut streams = exec.start()?;

    // Restores the terminal when dropped: normal return, `?`, or panic unwind.
    // Fatal signals are covered inside `RawMode`.
    let _raw = if args.tty {
        RawMode::enter().ok()
    } else {
        None
    };
    if args.tty {
        watch_resizes(exec.clone());
    }

    if let Some(mut stdin) = streams.stdin.take() {
        // Detached: a blocked stdin read must not keep vmctl from exiting.
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            let mut input = io::stdin().lock();
            while let Ok(n) = input.read(&mut buf) {
                if n == 0 || stdin.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
            // Dropping `stdin` sends EOF.
        });
    }

    let mut stderr = std::mem::replace(&mut streams.stderr, empty_output());
    let err_thread = std::thread::spawn(move || copy(&mut stderr, &mut io::stderr()));
    copy(&mut streams.stdout, &mut io::stdout());
    let _ = err_thread.join();

    let status = streams.wait()?;
    Ok(status.exit_code as u8)
}

fn empty_output() -> libclankerd::ExecOutput {
    libclankerd::ExecOutput::empty()
}

/// Copies until EOF, flushing each chunk (terminals must not wait for a newline).
/// Write errors (a closed pipe) are ignored so the stream is still drained.
fn copy(from: &mut impl Read, to: &mut impl Write) {
    let mut buf = [0u8; 16 * 1024];
    let mut writable = true;
    while let Ok(n) = from.read(&mut buf) {
        if n == 0 {
            break;
        }
        if writable {
            writable = to.write_all(&buf[..n]).and_then(|()| to.flush()).is_ok();
        }
    }
}

fn isatty(fd: i32) -> bool {
    // SAFETY: isatty only inspects the descriptor.
    unsafe { libc::isatty(fd) == 1 }
}

fn window_size() -> Option<(u16, u16)> {
    for fd in [libc::STDOUT_FILENO, libc::STDIN_FILENO] {
        // SAFETY: TIOCGWINSZ writes a winsize through a valid pointer.
        let ws = unsafe {
            let mut ws = std::mem::zeroed::<libc::winsize>();
            (libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0).then_some(ws)
        };
        if let Some(ws) = ws.filter(|w| w.ws_row > 0 && w.ws_col > 0) {
            return Some((ws.ws_row, ws.ws_col));
        }
    }
    None
}

static WINCH: AtomicBool = AtomicBool::new(false);

extern "C" fn on_winch(_: libc::c_int) {
    WINCH.store(true, Ordering::Relaxed);
}

/// Forwards SIGWINCH as exec resizes.
fn watch_resizes(exec: libclankerd::Exec) {
    // SAFETY: the handler only stores to an atomic.
    unsafe { libc::signal(libc::SIGWINCH, on_winch as *const () as libc::sighandler_t) };
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_millis(50));
            if WINCH.swap(false, Ordering::Relaxed)
                && let Some((rows, cols)) = window_size()
            {
                let _ = exec.resize(rows, cols);
            }
        }
    });
}

static SAVED: OnceLock<libc::termios> = OnceLock::new();

/// Puts the controlling terminal in raw mode; restores it on drop and when
/// vmctl is terminated by SIGTERM, SIGHUP or SIGQUIT.
struct RawMode;

impl RawMode {
    fn enter() -> io::Result<RawMode> {
        // SAFETY: termios calls on stdin with properly initialised structs.
        unsafe {
            let mut t = std::mem::zeroed::<libc::termios>();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut t) != 0 {
                return Err(io::Error::last_os_error());
            }
            let _ = SAVED.set(t);
            for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT] {
                libc::signal(sig, restore_and_die as *const () as libc::sighandler_t);
            }
            libc::cfmakeraw(&mut t);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &t) != 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(RawMode)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        restore();
    }
}

fn restore() {
    if let Some(t) = SAVED.get() {
        // SAFETY: tcsetattr is async-signal-safe and `t` is a valid termios.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, t) };
    }
}

extern "C" fn restore_and_die(sig: libc::c_int) {
    restore();
    // SAFETY: async-signal-safe; re-raise with the default action.
    unsafe {
        libc::signal(sig, libc::SIG_DFL);
        libc::raise(sig);
    }
}
