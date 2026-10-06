//! CLI seam: `vmctl exec` against a machine on the local-process VMM stand-in,
//! including a real pseudo-terminal for `-it` and the terminal-restore guarantees.

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

fn guestd() -> PathBuf {
    // Cargo builds only this package's binaries for these tests.
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut cmd = Command::new(cargo);
    cmd.args(["build", "-q", "-p", "clankerd-guestd"]);
    if profile_dir.file_name().is_some_and(|n| n == "release") {
        cmd.arg("--release");
    }
    assert!(cmd.status().unwrap().success());
    profile_dir.join("clankerd-guestd")
}

struct Env {
    dir: tempfile::TempDir,
    guestd: PathBuf,
}

impl Env {
    /// A temp state dir with one running machine called `m`.
    fn new() -> Env {
        let env = Env {
            dir: tempfile::Builder::new().prefix("vx").tempdir().unwrap(),
            guestd: guestd(),
        };
        let out = env.vmctl(&["create", "--name", "m", "img", "sleep", "600"]);
        assert!(out.status.success(), "{out:?}");
        assert!(env.vmctl(&["start", "m"]).status.success());
        env
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_vmctl"));
        c.args(args)
            .env("CLANKERD_STATE_DIR", self.dir.path().join("state"))
            .env("CLANKERD_RUNTIME_DIR", self.dir.path().join("run"))
            .env("CLANKERD_DEV_GUESTD", &self.guestd);
        c
    }

    fn vmctl(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }
}

#[test]
fn exec_separates_streams_and_returns_the_exit_code() {
    let env = Env::new();
    let out = env.vmctl(&["exec", "m", "sh", "-c", "echo out; echo err >&2; exit 4"]);
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(out.stdout, b"out\n");
    assert_eq!(out.stderr, b"err\n");

    let out = env.vmctl(&["exec", "m", "sh", "-c", "kill -9 $$"]);
    assert_eq!(out.status.code(), Some(137));
}

#[test]
fn exec_dash_i_pipes_stdin_through_to_eof() {
    let env = Env::new();
    let mut child = env
        .command(&["exec", "-i", "m", "sh", "-c", "cat; echo done"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"a\nb\n").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.stdout, b"a\nb\ndone\n");
    assert!(out.status.success());
}

#[test]
fn exec_in_a_stopped_or_missing_machine_fails_cleanly() {
    let env = Env::new();
    assert_eq!(
        env.vmctl(&["exec", "nope", "true"]).status.code(),
        Some(125)
    );
    assert!(env.vmctl(&["rm", "-f", "m"]).status.success());
    let out = env.vmctl(&["exec", "m", "true"]);
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Error"));
}

/// A pseudo-terminal pair; vmctl gets the slave as its controlling terminal.
struct Pty {
    master: File,
    slave: OwnedFd,
}

impl Pty {
    fn new(rows: u16, cols: u16) -> Pty {
        let (mut m, mut s) = (-1, -1);
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: openpty fills fds we then own.
        unsafe {
            assert_eq!(
                libc::openpty(
                    &mut m,
                    &mut s,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::from_ref(&ws).cast_mut(),
                ),
                0
            );
            Pty {
                master: File::from_raw_fd(m),
                slave: OwnedFd::from_raw_fd(s),
            }
        }
    }

    fn spawn(&self, mut cmd: Command) -> Child {
        let slave = self.slave.as_raw_fd();
        cmd.stdin(self.slave.try_clone().unwrap())
            .stdout(self.slave.try_clone().unwrap())
            .stderr(self.slave.try_clone().unwrap());
        // SAFETY: setsid/ioctl are async-signal-safe.
        unsafe {
            cmd.pre_exec(move || {
                libc::setsid();
                libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
                Ok(())
            });
        }
        cmd.spawn().unwrap()
    }

    fn set_size(&self, rows: u16, cols: u16) {
        let ws = libc::winsize {
            ws_row: rows,
            ws_col: cols,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: valid fd and pointer.
        unsafe { libc::ioctl(self.master.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
    }

    fn is_cooked(&self) -> bool {
        // SAFETY: tcgetattr fills a termios from a valid fd.
        let t = unsafe {
            let mut t = std::mem::zeroed::<libc::termios>();
            assert_eq!(libc::tcgetattr(self.slave.as_raw_fd(), &mut t), 0);
            t
        };
        t.c_lflag & (libc::ICANON | libc::ECHO | libc::ISIG)
            == (libc::ICANON | libc::ECHO | libc::ISIG)
            && t.c_oflag & libc::OPOST != 0
    }

    /// Reads from the master until `needle` appears.
    fn read_until(&mut self, needle: &str) -> String {
        let mut seen = Vec::new();
        let mut byte = [0u8; 1];
        let deadline = Instant::now() + Duration::from_secs(20);
        while !String::from_utf8_lossy(&seen).contains(needle) {
            assert!(Instant::now() < deadline, "no {needle:?} in {seen:?}");
            match self.master.read(&mut byte) {
                Ok(1) => seen.push(byte[0]),
                _ => break,
            }
        }
        String::from_utf8_lossy(&seen).into_owned()
    }
}

fn wait_for_exit(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        assert!(Instant::now() < deadline, "vmctl did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn exec_it_runs_in_a_terminal_and_restores_it_on_normal_exit() {
    let env = Env::new();
    let mut pty = Pty::new(31, 97);
    assert!(pty.is_cooked());
    let mut child = pty.spawn(env.command(&["exec", "-it", "m", "sh", "-c", "stty size; exit 3"]));
    let out = pty.read_until("31 97");
    assert!(out.contains("31 97"), "{out:?}");
    assert_eq!(wait_for_exit(&mut child).code(), Some(3));
    assert!(pty.is_cooked(), "terminal left in raw mode");
}

#[test]
fn exec_it_forwards_keystrokes_and_window_resizes() {
    let env = Env::new();
    let mut pty = Pty::new(24, 80);
    let script = "trap 'stty size; exit 0' WINCH; echo ready; while :; do sleep 0.05; done";
    let mut child = pty.spawn(env.command(&["exec", "-it", "m", "sh", "-c", script]));
    pty.read_until("ready");
    pty.set_size(50, 132);
    let out = pty.read_until("50 132");
    assert!(out.contains("50 132"), "{out:?}");
    assert_eq!(wait_for_exit(&mut child).code(), Some(0));
    assert!(pty.is_cooked());

    // Ctrl-C is a plain byte through the raw terminal and interrupts the guest process.
    let mut child = pty.spawn(env.command(&[
        "exec",
        "-it",
        "m",
        "sh",
        "-c",
        "echo ready2; exec sleep 600",
    ]));
    pty.read_until("ready2");
    pty.master.write_all(&[0x03]).unwrap();
    assert_eq!(wait_for_exit(&mut child).code(), Some(130));
    assert!(pty.is_cooked());
}

#[test]
fn exec_it_restores_the_terminal_when_killed_and_when_the_guest_disappears() {
    let env = Env::new();
    let mut pty = Pty::new(24, 80);

    let mut child = pty.spawn(env.command(&["exec", "-it", "m", "sh", "-c", "echo up; sleep 600"]));
    pty.read_until("up");
    assert!(!pty.is_cooked(), "expected raw mode while running");
    // SAFETY: plain signal delivery to our child.
    unsafe { libc::kill(child.id() as i32, libc::SIGTERM) };
    wait_for_exit(&mut child);
    assert!(pty.is_cooked(), "SIGTERM left the terminal raw");

    let mut child =
        pty.spawn(env.command(&["exec", "-it", "m", "sh", "-c", "echo up2; sleep 600"]));
    pty.read_until("up2");
    assert!(env.vmctl(&["rm", "-f", "m"]).status.success());
    let status = wait_for_exit(&mut child);
    assert!(!status.success());
    assert!(pty.is_cooked(), "losing the guest left the terminal raw");
}

#[test]
fn exec_dash_t_without_a_terminal_is_refused() {
    let env = Env::new();
    let out = env.vmctl(&["exec", "-t", "m", "true"]);
    assert_eq!(out.status.code(), Some(125));
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a TTY"));
}
