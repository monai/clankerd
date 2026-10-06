//! Seam A: the Docker exec lifecycle through the public library API only.

mod common;

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use common::*;
use libclankerd::{Engine, ErrorKind, Exec, ExecConfig, ExecStreams, Machine};

fn cfg(script: &str) -> ExecConfig {
    ExecConfig {
        cmd: vec!["/bin/sh".into(), "-c".into(), script.into()],
        ..Default::default()
    }
}

fn tty(script: &str) -> ExecConfig {
    ExecConfig {
        tty: true,
        attach_stdin: true,
        ..cfg(script)
    }
}

/// A running machine whose main process just sleeps, ready for exec.
fn running(engine: &Engine, name: &str) -> Machine {
    let m = create(engine, name, "sleep 600");
    m.start().unwrap();
    m
}

fn read_all(r: &mut impl Read) -> Vec<u8> {
    let mut v = Vec::new();
    r.read_to_end(&mut v).unwrap();
    v
}

/// Reads until `needle` shows up (or the stream ends), returning what was read.
fn read_until(r: &mut impl Read, needle: &str) -> String {
    let mut seen = Vec::new();
    let mut byte = [0u8; 1];
    while !String::from_utf8_lossy(&seen).contains(needle) {
        if r.read(&mut byte).unwrap() == 0 {
            break;
        }
        seen.push(byte[0]);
    }
    String::from_utf8_lossy(&seen).into_owned()
}

fn wait_running(exec: &Exec) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !exec.inspect().unwrap().running {
        assert!(Instant::now() < deadline, "exec never started");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn started(m: &Machine, config: ExecConfig) -> (Exec, ExecStreams) {
    let exec = m.exec_create(config).unwrap();
    let streams = exec.start().unwrap();
    (exec, streams)
}

#[test]
fn exec_returns_stdout_and_exit_code() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e1");
    let (exec, mut streams) = started(&m, cfg("echo hello; exit 7"));
    let status = streams.wait().unwrap();
    assert_eq!(status.exit_code, 7);
    assert_eq!(status.signal, None);
    assert_eq!(read_all(&mut streams.stdout), b"hello\n");
    let info = exec.inspect().unwrap();
    assert!(!info.running);
    assert_eq!(info.exit_code, Some(7));
}

#[test]
fn non_tty_separates_stdout_and_stderr_and_honours_stdin_eof() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e2");
    let config = ExecConfig {
        attach_stdin: true,
        ..cfg("cat; echo to-err >&2")
    };
    let (_exec, mut streams) = started(&m, config);
    let mut stdin = streams.stdin.take().unwrap();
    stdin.write_all(b"line one\n").unwrap();
    stdin.write_all(b"line two\n").unwrap();
    stdin.close(); // EOF: without it `cat` would never finish
    let status = streams.wait().unwrap();
    assert_eq!(status.exit_code, 0);
    assert_eq!(read_all(&mut streams.stdout), b"line one\nline two\n");
    assert_eq!(read_all(&mut streams.stderr), b"to-err\n");
}

#[test]
fn output_is_fully_drained_before_the_status() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e3");
    let (_exec, mut streams) = started(&m, cfg("head -c 3000000 /dev/zero; echo e >&2; exit 3"));
    // Wait first: every byte must already be queued when the status arrives.
    let status = streams.wait().unwrap();
    assert_eq!(status.exit_code, 3);
    assert_eq!(read_all(&mut streams.stdout).len(), 3_000_000);
    assert_eq!(read_all(&mut streams.stderr), b"e\n");
}

#[test]
fn kill_delivers_signals_and_reports_signal_deaths() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e4");

    // A process that handles SIGINT sees it and picks its own exit code.
    let (exec, mut streams) = started(
        &m,
        cfg("trap 'echo got-int; exit 5' INT; echo ready; while :; do sleep 0.05; done"),
    );
    read_until(&mut streams.stdout, "ready");
    exec.kill(libc::SIGINT).unwrap();
    assert_eq!(streams.wait().unwrap().exit_code, 5);
    assert_eq!(read_all(&mut streams.stdout), b"\ngot-int\n");

    // One that does not dies from the signal: 128 + signal, signal reported.
    let (exec, mut streams) = started(&m, cfg("exec sleep 600"));
    wait_running(&exec);
    exec.kill(libc::SIGTERM).unwrap();
    let status = streams.wait().unwrap();
    assert_eq!(status.signal, Some(libc::SIGTERM));
    assert_eq!(status.exit_code, 128 + libc::SIGTERM);
    assert_eq!(exec.inspect().unwrap().signal, Some(libc::SIGTERM));

    // A finished exec cannot be signalled.
    assert_eq!(exec.kill(libc::SIGTERM).unwrap_err().kind(), ErrorKind::Conflict);
}

#[test]
fn tty_exec_has_a_terminal_with_the_requested_size_and_resizes() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e5");
    let config = ExecConfig {
        size: Some((10, 20)),
        ..tty("test -t 0 && test -t 1 && echo is-tty; stty size; read x; stty size")
    };
    let (exec, mut streams) = started(&m, config);
    let mut stdin = streams.stdin.take().unwrap();
    let first = read_until(&mut streams.stdout, "10 20");
    assert!(first.contains("is-tty"), "{first:?}");
    exec.resize(33, 101).unwrap();
    stdin.write_all(b"\n").unwrap();
    assert_eq!(streams.wait().unwrap().exit_code, 0);
    let rest = String::from_utf8(read_all(&mut streams.stdout)).unwrap();
    assert!(rest.contains("33 101"), "{rest:?}");
}

#[test]
fn tty_ctrl_c_interrupts_the_foreground_process() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e6");
    let (_exec, mut streams) = started(&m, tty("echo ready; exec sleep 600"));
    let mut stdin = streams.stdin.take().unwrap();
    read_until(&mut streams.stdout, "ready");
    stdin.write_all(&[0x03]).unwrap();
    let status = streams.wait().unwrap();
    assert_eq!(status.signal, Some(libc::SIGINT));
    assert_eq!(status.exit_code, 130);
}

#[test]
fn tty_stdin_eof_ends_cat() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e7");
    let (_exec, mut streams) = started(&m, tty("cat"));
    let mut stdin = streams.stdin.take().unwrap();
    stdin.write_all(b"ping\n").unwrap();
    assert!(read_until(&mut streams.stdout, "ping").contains("ping"));
    stdin.close();
    assert_eq!(streams.wait().unwrap().exit_code, 0);
}

#[test]
fn tty_runs_a_full_screen_program() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e8");
    let target = env.root().join("edited.txt");
    let config = ExecConfig {
        size: Some((24, 80)),
        ..tty(&format!("exec vi -u NONE -n {}", target.display()))
    };
    let (exec, mut streams) = started(&m, config);
    let mut stdin = streams.stdin.take().unwrap();
    // vi draws its screen with escape sequences; wait for the first one.
    read_until(&mut streams.stdout, "\x1b[");
    exec.resize(40, 120).unwrap();
    stdin.write_all(b"ihello from vi\x1b:wq\r").unwrap();
    assert_eq!(streams.wait().unwrap().exit_code, 0);
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello from vi\n");
}

#[test]
fn four_sessions_run_concurrently_and_independently() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e9");
    // Each session blocks until all four are alive, so they must overlap.
    let gate = env.root().join("gate");
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let m = m.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                let script = format!(
                    "touch {g}.{i}; while [ $(ls {g}.* | wc -l) -lt 4 ]; do sleep 0.05; done; \
                     read line; echo \"s{i}:$line\"; echo err{i} >&2; exit {i}",
                    g = gate.display()
                );
                let config = ExecConfig {
                    attach_stdin: true,
                    ..cfg(&script)
                };
                let (_exec, mut streams) = started(&m, config);
                let mut stdin = streams.stdin.take().unwrap();
                writeln!(stdin, "input{i}").unwrap();
                let status = streams.wait().unwrap();
                assert_eq!(status.exit_code, i);
                assert_eq!(read_all(&mut streams.stdout), format!("s{i}:input{i}\n").as_bytes());
                assert_eq!(read_all(&mut streams.stderr), format!("err{i}\n").as_bytes());
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

#[test]
fn user_resolution_runs_as_the_requested_user() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e10");
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    let config = ExecConfig {
        user: uid.to_string(),
        ..cfg("id -u")
    };
    let (_exec, mut streams) = started(&m, config);
    assert_eq!(streams.wait().unwrap().exit_code, 0);
    assert_eq!(read_all(&mut streams.stdout), format!("{uid}\n").as_bytes());

    let config = ExecConfig {
        user: "no-such-user-here".into(),
        ..cfg("id -u")
    };
    let (_exec, mut streams) = started(&m, config);
    assert_eq!(streams.wait().unwrap().exit_code, 126);
    let err = String::from_utf8(read_all(&mut streams.stderr)).unwrap();
    assert!(err.contains("no-such-user-here"), "{err}");
}

#[test]
fn env_and_working_dir_apply_and_missing_binary_is_127() {
    let env = Env::new();
    let engine = env.engine();
    let m = running(&engine, "e11");
    let config = ExecConfig {
        env: vec!["GREETING=hi".into()],
        working_dir: "/tmp".into(),
        ..cfg("echo $GREETING; pwd")
    };
    let (_exec, mut streams) = started(&m, config);
    streams.wait().unwrap();
    assert_eq!(read_all(&mut streams.stdout), b"hi\n/tmp\n");

    let config = ExecConfig {
        cmd: vec!["/no/such/binary".into()],
        ..Default::default()
    };
    let (_exec, mut streams) = started(&m, config);
    assert_eq!(streams.wait().unwrap().exit_code, 127);
}

#[test]
fn lifecycle_errors_use_docker_kinds() {
    let env = Env::new();
    let engine = env.engine();
    let stopped = create(&engine, "idle", "exit 0");
    let err = stopped.exec_create(cfg("true")).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Conflict);

    let m = running(&engine, "e12");
    let err = m.exec_create(ExecConfig::default()).unwrap_err();
    assert_eq!(err.kind(), ErrorKind::InvalidParameter);

    let (exec, mut streams) = started(&m, cfg("true"));
    streams.wait().unwrap();
    assert_eq!(exec.start().err().unwrap().kind(), ErrorKind::Conflict);
}
