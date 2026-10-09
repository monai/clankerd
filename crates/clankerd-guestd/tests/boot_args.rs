//! guestd as booted by clankerd-vmspawn: no exit file (the boot directory is
//! read-only), the workload read from a config file, and a clear error when
//! there is nowhere to listen.

use std::io::BufReader;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

use clankerd_proto::guest::{
    Event, ExitAcknowledgment, ExitObserver, METHOD_ACKNOWLEDGE_EXIT, METHOD_EVENTS,
    METHOD_STARTUP_COMPLETE,
};
use clankerd_proto::varlink::{self, Call, Reply};

fn config(dir: &std::path::Path, script: &str) -> std::path::PathBuf {
    let path = dir.join("workload.json");
    let w = serde_json::json!({"argv": ["/bin/sh", "-c", script]});
    std::fs::write(&path, w.to_string()).unwrap();
    path
}

#[test]
fn runs_without_an_exit_file_and_reports_exit_over_events() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("g.sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let fd = listener.as_raw_fd();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_clankerd-guestd"));
    cmd.arg("--config")
        .arg(config(dir.path(), "exit 7"))
        .env("LISTEN_FDS", "1")
        .stdin(Stdio::null());
    // SAFETY: dup2 is async-signal-safe.
    unsafe {
        cmd.pre_exec(move || {
            if fd == 3 {
                let flags = libc::fcntl(3, libc::F_GETFD);
                libc::fcntl(3, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            } else if libc::dup2(fd, 3) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();

    // A slow observer must still recover an immediate exit beyond the old 300 ms linger.
    std::thread::sleep(std::time::Duration::from_millis(350));
    assert!(child.try_wait().unwrap().is_none());

    let mut conn = UnixStream::connect(&sock).unwrap();
    varlink::write(
        &mut conn,
        &Call {
            method: METHOD_EVENTS.into(),
            parameters: serde_json::Value::Null,
            more: true,
            upgrade: false,
        },
    )
    .unwrap();
    let mut reader = BufReader::new(conn);
    let mut last = None;
    while let Some(reply) = varlink::read::<Reply, _>(&mut reader).unwrap() {
        last = Some(serde_json::from_value::<Event>(reply.parameters).unwrap());
        if !reply.continues {
            break;
        }
    }
    assert_eq!(last, Some(Event::Exited { exit_code: 7 }));
    let rpc = |method: &str, parameters| {
        let mut connection = UnixStream::connect(&sock).unwrap();
        varlink::write(
            &mut connection,
            &Call {
                method: method.into(),
                parameters,
                more: false,
                upgrade: false,
            },
        )
        .unwrap();
        varlink::read::<Reply, _>(&mut BufReader::new(connection))
            .unwrap()
            .unwrap()
    };
    let bad = rpc(
        METHOD_ACKNOWLEDGE_EXIT,
        serde_json::to_value(ExitAcknowledgment {
            exit_code: 3,
            observer: ExitObserver::Recorded,
        })
        .unwrap(),
    );
    assert!(bad.error.is_some());
    assert!(child.try_wait().unwrap().is_none());
    assert!(
        rpc(METHOD_STARTUP_COMPLETE, serde_json::json!({}))
            .error
            .is_none()
    );
    assert!(child.try_wait().unwrap().is_none());
    clankerd_proto::guest::acknowledge_exit(
        &sock,
        ExitAcknowledgment {
            exit_code: 7,
            observer: ExitObserver::Recorded,
        },
    )
    .unwrap();
    assert!(child.wait().unwrap().success());
}

#[test]
fn without_a_listening_socket_it_explains_what_it_needs() {
    let dir = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_clankerd-guestd"))
        .arg("--config")
        .arg(config(dir.path(), "exit 0"))
        .env_remove("LISTEN_FDS")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("LISTEN_FDS") && err.contains("--vsock-port"),
        "{err}"
    );
}
