//! Seam A over the real production path: Engine -> VmspawnVmm -> the
//! clankerd-vmspawn helper process -> guestd from a boot directory.
//!
//! Only the hypervisor is replaced: on Linux `clankerd-vmspawn --dev-local`
//! runs the boot directory's guestd as a local process where macOS would call
//! libkrun. Failure cases use scripts standing in for a vmspawn that dies.

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use libclankerd::vmm::VmspawnVmm;
use libclankerd::{ErrorKind, Status};

fn dev_vmm() -> Arc<VmspawnVmm> {
    Arc::new(VmspawnVmm::new(vmspawn_path(), guestd_path()).dev_local())
}

fn script(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join("fake-vmspawn");
    std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[test]
fn run_returns_the_exit_code_through_the_helper() {
    let env = Env::new();
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    let m = create(&engine, "viahelper", "echo booted; exit 5");
    m.start().unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 5);
    assert_eq!(m.inspect().unwrap().state.status, Status::Exited);
}

#[test]
fn logs_show_what_the_guest_printed() {
    let env = Env::new();
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    let m = create(&engine, "chatty", "echo from-the-guest");
    m.start().unwrap();
    m.wait().unwrap();
    let logs = String::from_utf8(m.logs().unwrap()).unwrap();
    assert!(logs.contains("from-the-guest"), "{logs}");
}

#[test]
fn the_helper_outlives_the_library_process() {
    let env = Env::new();
    let marker = env.root().join("go");
    {
        let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
        let m = create(
            &engine,
            "survivor",
            &format!(
                "while [ ! -e {} ]; do sleep 0.05; done; exit 9",
                marker.display()
            ),
        );
        m.start().unwrap();
    }
    // A new Engine stands in for a restarted library process.
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    let m = engine.get("survivor").unwrap();
    assert_eq!(m.inspect().unwrap().state.status, Status::Running);
    std::fs::write(&marker, "").unwrap();
    assert_eq!(m.wait().unwrap().exit_code, 9);
}

#[test]
fn missing_libkrun_fails_fast_with_install_instructions() {
    let env = Env::new();
    let fake = script(
        env.root(),
        "echo 'dyld[42]: Library not loaded: /opt/homebrew/lib/libkrun.1.dylib' >&2; exit 1",
    );
    let vmm = Arc::new(VmspawnVmm::new(fake, guestd_path()));
    let mut cfg = env.config(vmm);
    cfg.start_timeout = Duration::from_secs(20);
    let engine = libclankerd::Engine::new(cfg).unwrap();
    let m = create(&engine, "nolib", "exit 0");

    let began = Instant::now();
    let err = m.start().unwrap_err();
    assert!(
        began.elapsed() < Duration::from_secs(10),
        "should not wait for the start timeout"
    );
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(err.message().contains("libkrun is not installed"), "{err}");
    assert!(err.message().contains("brew"), "{err}");
    assert_eq!(m.inspect().unwrap().state.status, Status::Created);
}

#[test]
fn helper_failure_text_reaches_the_caller() {
    let env = Env::new();
    let fake = script(
        env.root(),
        "echo 'clankerd-vmspawn: the hypervisor entitlement is missing: sign it' >&2; exit 1",
    );
    let engine =
        libclankerd::Engine::new(env.config(Arc::new(VmspawnVmm::new(fake, guestd_path()))))
            .unwrap();
    let m = create(&engine, "noent", "exit 0");
    let err = m.start().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(
        err.message().contains("hypervisor entitlement is missing"),
        "{err}"
    );
}

#[test]
fn a_silent_workload_helper_crash_preserves_its_signal() {
    let env = Env::new();
    let fake = script(env.root(), "kill -TERM $$");
    let engine =
        libclankerd::Engine::new(env.config(Arc::new(VmspawnVmm::new(fake, guestd_path()))))
            .unwrap();
    let m = create(&engine, "silent", "exit 0");
    let err = m.start().unwrap_err();
    assert!(err.message().contains("workload boot:"), "{err}");
    let status_file = env
        .root()
        .join("state/machines")
        .join(m.id())
        .join("vmspawn-exit");
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        if let Ok(status) = std::fs::read_to_string(&status_file) {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "helper exit status was not recorded"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(status.contains("signal: 15"), "{status}");
}

#[test]
fn guest_console_errors_explain_a_helper_that_exits_successfully() {
    let env = Env::new();
    let fake = script(
        env.root(),
        "echo 'clankerd-guestd: mount ext4 /dev/vda: Invalid argument' > \"$(dirname \"$2\")/console.log\"; exit 0",
    );
    let engine =
        libclankerd::Engine::new(env.config(Arc::new(VmspawnVmm::new(fake, guestd_path()))))
            .unwrap();
    let m = create(&engine, "guest-failed", "exit 0");
    let err = m.start().unwrap_err();
    assert!(
        err.message()
            .contains("mount ext4 /dev/vda: Invalid argument"),
        "{err}"
    );
    assert!(!err.message().contains("may have crashed"), "{err}");
}

#[test]
fn stale_libkrun_listener_is_removed_before_the_next_helper_starts() {
    let env = Env::new();
    let fake = script(env.root(), "exit 1");
    let engine =
        libclankerd::Engine::new(env.config(Arc::new(VmspawnVmm::new(&fake, guestd_path()))))
            .unwrap();
    let machine = create(&engine, "restart", "sleep 60");
    let socket = env
        .root()
        .join("run")
        .join(format!("{}.vsock", &machine.id()[..12]));
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    std::fs::write(&fake, format!(
        "#!/bin/sh\nif [ -e '{}' ]; then echo 'stale listener remains' >&2; else echo 'listener cleanup verified' >&2; fi\nexit 1\n",
        socket.display()
    )).unwrap();
    let error = machine.start().unwrap_err();
    assert!(
        error.message().contains("listener cleanup verified"),
        "{error}"
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn the_real_helper_off_macos_explains_it_cannot_boot() {
    let env = Env::new();
    let vmm = Arc::new(VmspawnVmm::new(vmspawn_path(), guestd_path()));
    let engine = libclankerd::Engine::new(env.config(vmm)).unwrap();
    let m = create(&engine, "linux", "exit 0");
    let err = m.start().unwrap_err();
    assert_eq!(err.kind(), ErrorKind::Unavailable);
    assert!(err.message().contains("only supported on macOS"), "{err}");
}

#[test]
fn published_ports_work_through_the_helper_proxy() {
    use libclankerd::PortBinding;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener, TcpStream};

    let env = Env::new();
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    let m = create(&engine, "ported", "sleep 60");
    m.start().unwrap();

    // The dev-local guest's loopback is 127.0.0.2.
    let service = TcpListener::bind(("127.0.0.2", 0)).unwrap();
    let guest_port = service.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for mut c in service.incoming().flatten() {
            let mut seen = String::new();
            c.read_to_string(&mut seen).unwrap();
            write!(c, "guest saw {seen}").unwrap();
        }
    });

    let published = m.publish(PortBinding::loopback(0, guest_port)).unwrap();
    let mut conn = TcpStream::connect(published.local_addr()).unwrap();
    conn.write_all(b"ping").unwrap();
    conn.shutdown(Shutdown::Write).unwrap();
    let mut out = String::new();
    conn.read_to_string(&mut out).unwrap();
    assert_eq!(out, "guest saw ping");
    let addr = published.local_addr();
    let conflict = m
        .publish(PortBinding::loopback(addr.port(), guest_port))
        .err()
        .unwrap();
    assert_eq!(conflict.kind(), ErrorKind::Conflict);
    drop(published);
    assert!(TcpStream::connect(addr).is_err());
    let _republished = m
        .publish(PortBinding::loopback(addr.port(), guest_port))
        .unwrap();
    m.remove(true).unwrap();
}

fn spawn_spec(env: &Env, id: &str) -> serde_json::Value {
    let path = env
        .root()
        .join("state/machines")
        .join(id)
        .join("vmspawn.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

#[test]
fn a_machine_with_a_root_disk_hands_it_to_the_helper() {
    use common::registry::{Registry, layer};

    let env = Env::new();
    let registry = Registry::start();
    registry.push("test/app", "v1", &[layer(&[("hello", b"hi")])]);
    let mut cfg = env.config(dev_vmm());
    cfg.cache_dir = Some(env.root().join("cache"));
    cfg.populator = Some(Arc::new(TarCapture::default()));
    cfg.insecure_registries = vec![registry.addr.clone()];
    let engine = libclankerd::Engine::new(cfg).unwrap();
    let m = engine
        .create(
            None,
            libclankerd::MachineConfig {
                image: format!("{}/test/app:v1", registry.addr),
                cmd: vec!["/bin/true".into()],
                ..Default::default()
            },
            libclankerd::HostConfig::default(),
        )
        .unwrap();
    m.start().unwrap();
    m.wait().unwrap();

    let spec = spawn_spec(&env, m.id());
    let disk = env
        .root()
        .join("state/machines")
        .join(m.id())
        .join(libclankerd::ROOT_DISK);
    assert_eq!(spec["root_disk"], disk.to_str().unwrap());
    assert_eq!(spec["populate"], false);
}

#[test]
fn a_machine_without_an_image_boots_from_the_boot_directory_alone() {
    let env = Env::new();
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    let m = create(&engine, "bootdir", "exit 0");
    m.start().unwrap();
    m.wait().unwrap();
    assert!(spawn_spec(&env, m.id())["root_disk"].is_null());
}

#[test]
fn startup_acknowledgment_failure_leaves_a_terminal_retryable_machine() {
    use clankerd_proto::guest::{Event, METHOD_STARTUP_COMPLETE};
    use clankerd_proto::varlink::{self, Call, Reply};
    use libclankerd::Vmm;
    use libclankerd::vmm::{BootHandle, BootSpec, LocalProcessVmm};
    use std::os::unix::net::UnixListener;
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};

    struct AckFailure {
        first: AtomicBool,
        disconnect: bool,
        inner: LocalProcessVmm,
    }
    impl Vmm for AckFailure {
        fn boot(&self, spec: &BootSpec) -> libclankerd::Result<BootHandle> {
            if !self.first.swap(false, Ordering::SeqCst) {
                return self.inner.boot(spec);
            }
            let listener = UnixListener::bind(&spec.guest_socket)?;
            let disconnect = self.disconnect;
            std::thread::spawn(move || {
                let (mut events, _) = listener.accept().unwrap();
                let _: Call = varlink::read(&mut std::io::BufReader::new(&mut events))
                    .unwrap()
                    .unwrap();
                varlink::write(
                    &mut events,
                    &Reply {
                        parameters: serde_json::to_value(Event::Ready).unwrap(),
                        continues: true,
                        error: None,
                    },
                )
                .unwrap();
                let (mut connection, _) = listener.accept().unwrap();
                let call: Call = varlink::read(&mut std::io::BufReader::new(&mut connection))
                    .unwrap()
                    .unwrap();
                assert_eq!(call.method, METHOD_STARTUP_COMPLETE);
                if !disconnect {
                    varlink::write(
                        &mut connection,
                        &Reply {
                            parameters: serde_json::json!({"message": "fake startup failure"}),
                            continues: false,
                            error: Some("io.clankerd.Guest.Unavailable".into()),
                        },
                    )
                    .unwrap();
                }
            });
            let mut child = Command::new("sleep")
                .arg("60")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()?;
            let pid = child.id();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(BootHandle { pid })
        }
    }
    for disconnect in [false, true] {
        let env = Env::new();
        let vmm = Arc::new(AckFailure {
            first: AtomicBool::new(true),
            disconnect,
            inner: LocalProcessVmm::new(guestd_path()),
        });
        let engine = libclankerd::Engine::new(env.config(vmm)).unwrap();
        let machine = create(&engine, "retry", "exit 7");
        assert!(machine.start().is_err());
        let state = machine.inspect().unwrap().state;
        assert_eq!(state.status, Status::Dead);
        assert_eq!(state.pid, None);
        assert!(!state.error.is_empty());
        assert_eq!(machine.wait().unwrap().exit_code, -1);
        machine.start().unwrap();
        assert_eq!(machine.wait().unwrap().exit_code, 7);
    }
}

#[test]
fn forwarding_control_accepts_requests_after_connection_establishment() {
    use clankerd_proto::varlink::{self, Call, Reply};
    use std::io::Write;
    use std::os::unix::net::UnixStream;
    let env = Env::new();
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    let machine = create(&engine, "control", "sleep 60");
    machine.start().unwrap();
    let spec = spawn_spec(&env, machine.id());
    let socket = spec["forwarding"]["control_socket"].as_str().unwrap();
    for delay in [0, 30, 100] {
        let mut connection = UnixStream::connect(socket).unwrap();
        connection
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        std::thread::sleep(Duration::from_millis(delay));
        let request = serde_json::to_vec(&Call {
            method: "io.clankerd.Forwarding.Ready".into(),
            parameters: serde_json::json!({}),
            more: false,
            upgrade: false,
        })
        .unwrap();
        for chunk in request.chunks(8) {
            connection.write_all(chunk).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        connection.write_all(&[0]).unwrap();
        let reply: Reply = varlink::read(&mut std::io::BufReader::new(connection))
            .unwrap()
            .unwrap();
        assert_eq!(reply.error, None);
        assert_eq!(reply.parameters, serde_json::json!(true));
    }
    machine.stop(Duration::from_secs(10)).unwrap();
}

#[test]
fn root_disk_capacity_and_contents_survive_discard_and_restart() {
    use std::io::{Read, Write};
    for missing in [1, 4096, 65536] {
        let env = Env::new();
        let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
        let machine = create(&engine, "root", "sleep 60");
        let disk = env
            .root()
            .join("state/machines")
            .join(machine.id())
            .join(libclankerd::ROOT_DISK);
        let capacity = 32 * 1024 * 1024;
        let mut file = std::fs::File::create(&disk).unwrap();
        file.write_all(b"marker").unwrap();
        file.set_len(capacity).unwrap();
        machine.start().unwrap();
        for _ in 0..2 {
            machine.stop(Duration::from_secs(10)).unwrap();
            file.set_len(capacity - missing).unwrap();
            machine.start().unwrap();
            assert_eq!(std::fs::metadata(&disk).unwrap().len(), capacity);
            let mut marker = [0; 6];
            std::fs::File::open(&disk)
                .unwrap()
                .read_exact(&mut marker)
                .unwrap();
            assert_eq!(&marker, b"marker");
        }
        machine.stop(Duration::from_secs(10)).unwrap();
    }
}

#[test]
fn force_remove_finishes_machine_writers_before_deleting_state() {
    let env = Env::new();
    let engine = libclankerd::Engine::new(env.config(dev_vmm())).unwrap();
    for index in 0..100 {
        let machine = create(&engine, &format!("remove-{index}"), "sleep 60");
        machine.start().unwrap();
        let pid = machine.inspect().unwrap().state.pid.unwrap();
        let other =
            (index % 2 == 1).then(|| libclankerd::Engine::new(env.config(dev_vmm())).unwrap());
        let machine = match &other {
            Some(other) => other.get(machine.id()).unwrap(),
            None => machine,
        };
        let directory = env.root().join("state/machines").join(machine.id());
        machine.remove(true).unwrap_or_else(|error| {
            panic!(
                "force remove {index}: {error}; remaining={:?}",
                std::fs::read_dir(&directory).map(|entries| entries
                    .flatten()
                    .map(|entry| entry.file_name())
                    .collect::<Vec<_>>())
            )
        });
        // SAFETY: signal zero checks that forced removal finished the helper.
        assert_eq!(unsafe { libc::kill(pid as i32, 0) }, -1);
        assert!(!directory.exists());
        assert_eq!(
            engine
                .get(machine.id())
                .err()
                .expect("machine removed")
                .kind(),
            ErrorKind::NotFound
        );
    }
}
