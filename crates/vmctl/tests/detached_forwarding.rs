mod common;

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

struct Service {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Service {
    fn new(ip: Ipv4Addr) -> Self {
        let listener = TcpListener::bind((ip, 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::Relaxed) {
                if let Ok((mut connection, _)) = listener.accept() {
                    let _ = connection.write_all(b"hello");
                } else {
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
        });
        Self {
            addr,
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
    }
}

struct Rig {
    dir: tempfile::TempDir,
    guestd: PathBuf,
    helper: PathBuf,
}

impl Rig {
    fn new() -> Self {
        let guestd = common::guestd();
        let helper = common::built("clankerd-vmspawn");
        Self {
            dir: tempfile::Builder::new().prefix("fw").tempdir().unwrap(),
            guestd,
            helper,
        }
    }

    fn run(&self, args: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_vmctl"))
            .arg("--vmspawn")
            .arg(&self.helper)
            .arg("--guestd")
            .arg(&self.guestd)
            .args(args)
            .env_remove("CLANKERD_DEV_GUESTD")
            .env("CLANKERD_STATE_DIR", self.dir.path().join("state"))
            .env("CLANKERD_RUNTIME_DIR", self.dir.path().join("run"))
            .env("CLANKERD_BOOT_DIR_ROOT", "true")
            .env("CLANKERD_VMSPAWN_DEV_LOCAL", "1")
            .output()
            .unwrap()
    }

    fn ok(&self, args: &[&str]) -> Output {
        let output = self.run(args);
        assert!(
            output.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = self.run(&["rm", "-f", "box"]);
    }
}

fn fetch(addr: SocketAddr) {
    let mut connection = TcpStream::connect(addr).unwrap();
    connection
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    connection.shutdown(Shutdown::Write).unwrap();
    let mut reply = String::new();
    connection.read_to_string(&mut reply).unwrap();
    assert_eq!(reply, "hello");
}

#[test]
fn configured_ports_survive_the_starter_and_later_cli_processes_and_restart() {
    let service = Service::new(Ipv4Addr::new(127, 0, 0, 2));
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let addr = reservation.local_addr().unwrap();
    drop(reservation);
    let rig = Rig::new();
    let binding = format!("{}:{}", addr.port(), service.addr.port());
    rig.ok(&[
        "run",
        "-d",
        "--name",
        "box",
        "-p",
        &binding,
        "boot-dir",
        "/bin/sleep",
        "60",
    ]);
    fetch(addr);
    rig.ok(&["exec", "box", "/bin/true"]);
    fetch(addr);
    rig.ok(&["stop", "box"]);
    assert!(TcpStream::connect(addr).is_err());
    rig.ok(&["start", "box"]);
    fetch(addr);
}

#[test]
fn configured_host_gateway_survives_the_starter_and_later_cli_processes() {
    let service = Service::new(Ipv4Addr::LOCALHOST);
    let rig = Rig::new();
    let port = service.addr.port().to_string();
    rig.ok(&[
        "run",
        "-d",
        "--name",
        "box",
        "--host-gateway-port",
        &port,
        "boot-dir",
        "/bin/sleep",
        "60",
    ]);
    let python = format!(
        "import socket; print(socket.create_connection(('127.0.0.2', {}), 3).recv(64).decode())",
        service.addr.port()
    );
    for _ in 0..2 {
        let output = rig.ok(&["exec", "box", "python3", "-c", &python]);
        assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "hello");
    }
    rig.ok(&["stop", "box"]);
    rig.ok(&["start", "box"]);
    let output = rig.ok(&["exec", "box", "python3", "-c", &python]);
    assert_eq!(String::from_utf8(output.stdout).unwrap().trim(), "hello");
    assert_eq!(service.addr.ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
}
