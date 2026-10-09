//! The helper's logic: a hypervisor, a proxy from the inherited listener to
//! the guest's vsock socket, and a watcher that records the exit code.

use std::fs::{self, OpenOptions};
use std::io::BufReader;
use std::net::Shutdown;
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use clankerd_proto::guest::{Event, ExitAcknowledgment, ExitObserver, METHOD_EVENTS};
use clankerd_proto::spawn::{
    BOOT_GUESTD, BOOT_WORKLOAD, GUEST_VSOCK_PORT, HOST_VSOCK_PORT, SpawnSpec,
};
use clankerd_proto::varlink::{self, Call, Reply};

/// What actually runs the guest.
pub trait Hypervisor {
    /// Runs the guest to completion. Real hypervisors never return `Ok`
    /// (libkrun exits the process); a failure to start returns `Err`.
    fn boot(&self, spec: &SpawnSpec) -> Result<(), String>;
}

/// libkrun, with guestd as PID 1 from the boot directory.
pub struct Libkrun;

impl Hypervisor for Libkrun {
    fn boot(&self, spec: &SpawnSpec) -> Result<(), String> {
        let mut cfg = libkrun_sys::BootConfig::new(&spec.boot_dir, format!("/{BOOT_GUESTD}"));
        // libkrun's init passes these as the program's arguments (after argv[0]).
        cfg.argv = if spec.populate {
            // Population boot: no workload, only the root-disk methods.
            vec![
                "--populate".into(),
                "--vsock-port".into(),
                GUEST_VSOCK_PORT.to_string(),
            ]
        } else {
            let mut argv = vec![
                "--config".into(),
                format!("/{BOOT_WORKLOAD}"),
                "--vsock-port".into(),
                GUEST_VSOCK_PORT.to_string(),
                "--host-vsock-port".into(),
                HOST_VSOCK_PORT.to_string(),
            ];
            if spec.root_disk.is_some() {
                // Mount the root disk, pivot into it, then run the workload.
                argv.push("--boot".into());
            }
            argv
        };
        // Order matters: the first disk is /dev/vda (root), the next /dev/vdb.
        if let Some(disk) = &spec.root_disk {
            cfg.disks.push(libkrun_sys::Disk {
                block_id: "root".into(),
                path: disk.clone(),
                read_only: false,
            });
        }
        if let Some(disk) = &spec.volume_disk {
            cfg.disks.push(libkrun_sys::Disk {
                block_id: "volume".into(),
                path: disk.clone(),
                read_only: false,
            });
        }
        cfg.shares = spec
            .shares
            .iter()
            .map(|s| libkrun_sys::Share {
                tag: s.tag.clone(),
                path: s.path.clone(),
            })
            .collect();
        // Make init.krun exec guestd as PID 1 instead of forking it.
        cfg.env = vec!["KRUN_INIT_PID1=1".into()];

        cfg.console_log = Some(spec.console_log.clone());
        if let Some(net) = &spec.net
            && !spec.populate
        {
            cfg.nets = vec![libkrun_sys::NetDevice {
                socket: net.socket.clone(),
                mac: net.mac,
                features: net.features,
                vfkit: net.vfkit,
            }];
        }
        cfg.vsock_ports = vec![
            libkrun_sys::VsockPort {
                port: GUEST_VSOCK_PORT,
                host_socket: spec.vsock_socket.clone(),
                listen: true,
            },
            libkrun_sys::VsockPort {
                port: HOST_VSOCK_PORT,
                host_socket: spec.host_socket.clone(),
                listen: false,
            },
        ];
        cfg.cpus = spec.cpus;
        cfg.memory_mib = spec.memory_mib;
        libkrun_sys::boot(&cfg)
            .map(|never| match never {})
            .map_err(|e| e.to_string())
    }
}

/// Development stand-in: guestd from the boot directory as a local process,
/// serving on `vsock_socket` like libkrun would.
pub struct DevLocal;

impl Hypervisor for DevLocal {
    fn boot(&self, spec: &SpawnSpec) -> Result<(), String> {
        let _ = fs::remove_file(&spec.vsock_socket);
        let listener = UnixListener::bind(&spec.vsock_socket)
            .map_err(|e| format!("binding {}: {e}", spec.vsock_socket.display()))?;
        let fd = listener.as_raw_fd();
        let console = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&spec.console_log)
            .map_err(|e| e.to_string())?;
        let mut cmd = Command::new(spec.boot_dir.join(BOOT_GUESTD));
        if spec.populate {
            cmd.arg("--populate").arg("--boot-dir").arg(&spec.boot_dir);
            // SAFETY: geteuid has no preconditions.
            if unsafe { libc::geteuid() } != 0 {
                // Unprivileged development runs cannot chown or mount.
                cmd.arg("--lenient-ownership");
            }
        } else {
            cmd.arg("--config").arg(spec.boot_dir.join(BOOT_WORKLOAD));
        }
        cmd.arg("--host-socket")
            .arg(&spec.host_socket)
            // The local process shares the host's loopback, so the guest's
            // loopback is another address (same as LocalProcessVmm).
            .arg("--loopback")
            .arg("127.0.0.2")
            .env("LISTEN_FDS", "1")
            .stdin(Stdio::null())
            .stdout(console.try_clone().map_err(|e| e.to_string())?)
            .stderr(console);
        // SAFETY: only async-signal-safe calls (dup2, fcntl) between fork and exec.
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
        cmd.spawn()
            .and_then(|mut c| c.wait())
            .map_err(|e| format!("cannot run guestd: {e}"))?;
        drop(listener);
        let _ = fs::remove_file(&spec.vsock_socket);
        Ok(())
    }
}

/// Serves the machine: proxy connections from `listener` to the guest and
/// record its exit code, while `hypervisor` runs the guest.
pub fn run(
    spec: &SpawnSpec,
    listener: UnixListener,
    hypervisor: &dyn Hypervisor,
) -> Result<(), String> {
    let _forwarding = spec
        .forwarding
        .as_ref()
        .map(|config| {
            libclankerd::forwarding::ForwardingServer::start(
                config,
                spec.vsock_socket.clone(),
                spec.host_socket.clone(),
            )
            .map_err(|e| e.to_string())
        })
        .transpose()?;
    let guest = spec.vsock_socket.clone();
    std::thread::spawn(move || proxy(listener, guest));
    if !spec.populate {
        let (guest, exit_file) = (spec.vsock_socket.clone(), spec.exit_file.clone());
        std::thread::spawn(move || record_exit(&guest, &exit_file));
    }
    hypervisor.boot(spec)
}

/// Accepts connections and pipes each one to the guest's socket. A guest that
/// is not listening yet makes the connection close, which clients retry.
fn proxy(listener: UnixListener, guest: std::path::PathBuf) {
    for client in listener.incoming().flatten() {
        let guest = guest.clone();
        std::thread::spawn(move || {
            let Ok(upstream) = UnixStream::connect(&guest) else {
                return;
            };
            pipe(client, upstream);
        });
    }
}

fn pipe(a: UnixStream, b: UnixStream) {
    let (Ok(mut a_read), Ok(mut b_read)) = (a.try_clone(), b.try_clone()) else {
        return;
    };
    let (mut a_write, mut b_write) = (a, b);
    let forward = std::thread::spawn(move || {
        let _ = std::io::copy(&mut a_read, &mut b_write);
        let _ = b_write.shutdown(Shutdown::Write);
    });
    let _ = std::io::copy(&mut b_read, &mut a_write);
    let _ = a_write.shutdown(Shutdown::Write);
    let _ = forward.join();
}

/// Subscribes to the guest's events until it reports the workload's exit,
/// then writes the code to `exit_file`. Retries until the guest is up.
fn record_exit(guest: &Path, exit_file: &Path) {
    loop {
        if let Some(code) = wait_for_exit(guest) {
            let tmp = exit_file.with_extension("tmp");
            if let Err(error) =
                fs::write(&tmp, code.to_string()).and_then(|()| fs::rename(&tmp, exit_file))
            {
                eprintln!("clankerd-vmspawn: recording workload exit: {error}");
            } else if clankerd_proto::guest::acknowledge_exit(
                guest,
                ExitAcknowledgment {
                    exit_code: code,
                    observer: ExitObserver::Recorded,
                },
            )
            .is_ok()
            {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait_for_exit(guest: &Path) -> Option<i32> {
    let mut conn = UnixStream::connect(guest).ok()?;
    varlink::write(
        &mut conn,
        &Call {
            method: METHOD_EVENTS.into(),
            parameters: serde_json::Value::Null,
            more: true,
            upgrade: false,
        },
    )
    .ok()?;
    let mut reader = BufReader::new(conn);
    loop {
        let reply: Reply = varlink::read(&mut reader).ok()??;
        if reply.error.is_some() {
            return None;
        }
        if let Ok(Event::Exited { exit_code }) = serde_json::from_value(reply.parameters) {
            return Some(exit_code);
        }
    }
}
