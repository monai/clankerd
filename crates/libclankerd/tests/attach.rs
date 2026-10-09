//! Seam A: attaching to a machine's main process (`docker run -it`) through
//! the public library API only.

mod common;

use std::io::{Read, Write};

use common::*;
use libclankerd::{HostConfig, MachineConfig};

fn interactive(script: &str, tty: bool) -> MachineConfig {
    MachineConfig {
        tty,
        open_stdin: true,
        ..sh(script)
    }
}

#[test]
fn attached_status_and_output_survive_machine_shutdown() {
    for (script, code) in [
        ("printf output; exit 0", 0),
        ("printf output; exit 7", 7),
        ("printf output; kill -TERM $$", 143),
    ] {
        for late in [false, true] {
            let env = Env::new();
            let machine = env
                .engine()
                .create(
                    Some("attached"),
                    interactive(script, false),
                    HostConfig::default(),
                )
                .unwrap();
            let mut streams = machine.start_attached(None).unwrap();
            if late {
                assert_eq!(machine.wait().unwrap().exit_code, code);
            }
            assert_eq!(streams.wait().unwrap().exit_code, code);
            assert_eq!(streams.wait().unwrap().exit_code, code);
            let mut output = String::new();
            streams.stdout.read_to_string(&mut output).unwrap();
            assert_eq!(output, "output");
            assert_eq!(machine.wait().unwrap().exit_code, code);
        }
    }
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

#[test]
fn attach_gives_the_main_process_a_terminal_with_the_requested_size() {
    let env = Env::new();
    let engine = env.engine();
    let m = engine
        .create(
            Some("a1"),
            interactive(
                "test -t 0 && echo is-tty; read x; stty size; echo got:$x",
                true,
            ),
            HostConfig::default(),
        )
        .unwrap();
    m.start().unwrap();

    let mut streams = m.attach(Some((31, 97))).unwrap();
    let first = read_until(&mut streams.stdout, "is-tty");
    assert!(first.contains("is-tty"), "{first:?}");

    streams.stdin.as_mut().unwrap().write_all(b"hi\n").unwrap();
    let rest = read_until(&mut streams.stdout, "got:hi");
    assert!(
        rest.contains("31 97") && rest.contains("got:hi"),
        "{rest:?}"
    );
    assert_eq!(streams.wait().unwrap().exit_code, 0);
}

#[test]
fn attach_without_a_terminal_separates_stdout_and_stderr_and_honours_eof() {
    let env = Env::new();
    let engine = env.engine();
    let m = engine
        .create(
            Some("a2"),
            interactive("cat; echo done >&2; exit 3", false),
            HostConfig::default(),
        )
        .unwrap();
    m.start().unwrap();

    let mut streams = m.attach(None).unwrap();
    let mut stdin = streams.stdin.take().unwrap();
    stdin.write_all(b"ping\n").unwrap();
    stdin.close();
    let status = streams.wait().unwrap();
    let (mut out, mut err) = (String::new(), String::new());
    streams.stdout.read_to_string(&mut out).unwrap();
    streams.stderr.read_to_string(&mut err).unwrap();
    assert_eq!((out.as_str(), err.as_str()), ("ping\n", "done\n"));
    assert_eq!(status.exit_code, 3);
}

#[test]
fn output_written_before_attaching_is_replayed() {
    let env = Env::new();
    let engine = env.engine();
    let m = engine
        .create(
            Some("a3"),
            interactive("echo early; sleep 600", true),
            HostConfig::default(),
        )
        .unwrap();
    m.start().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let mut streams = m.attach(None).unwrap();
    assert!(read_until(&mut streams.stdout, "early").contains("early"));
    m.kill(libc::SIGKILL).unwrap();
}

#[test]
fn attach_to_a_machine_without_stdin_or_tty_is_refused() {
    let env = Env::new();
    let engine = env.engine();
    let m = create(&engine, "a4", "sleep 600");
    m.start().unwrap();
    let err = m.attach(None).err().expect("refused");
    assert_eq!(err.kind(), libclankerd::ErrorKind::Conflict);
}
