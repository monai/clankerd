//! A runtime-owned authorization session. It launches only this bundle's
//! networking executable and exits after the last attachment disconnects.
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{
            net::{UnixListener, UnixStream},
            process::CommandExt,
        },
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Serialize, Deserialize)]
struct Request {
    args: Vec<String>,
    input: String,
}
struct AuthorizedCode(PathBuf);
impl Drop for AuthorizedCode {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn socket(uid: u32) -> Result<PathBuf> {
    let binary = std::env::current_exe()?;
    let identity: String = Sha256::digest(binary.as_os_str().as_encoded_bytes())
        .iter()
        .take(8)
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok(PathBuf::from(format!(
        "/var/run/clankerd-auth-{uid}-{identity}.sock"
    )))
}
fn connect() -> Result<UnixStream> {
    if !cfg!(target_os = "macos") {
        return Err("native authorization requires macOS".into());
    }
    let uid = unsafe { libc::getuid() };
    if uid == 0 {
        return Err("run vmctl as your ordinary user".into());
    }
    let path = socket(uid)?;
    if let Ok(connection) = UnixStream::connect(&path) {
        return Ok(connection);
    }
    let status = Command::new("sudo")
        .arg("--")
        .arg(std::env::current_exe()?)
        .arg("authorization-session")
        .status()?;
    if !status.success() {
        return Err("administrator authorization was declined".into());
    }
    Ok(UnixStream::connect(path)?)
}
pub fn authorize() -> Result<()> {
    connect().map(|_| ())
}
pub fn launch(args: Vec<String>) -> Result<()> {
    let mut connection = connect()?;
    let mut input = String::new();
    if args.first().map(String::as_str) == Some("pf") {
        std::io::stdin().take(4097).read_to_string(&mut input)?;
    }
    let mut request = serde_json::to_vec(&Request { args, input })?;
    request.push(b'\n');
    connection.write_all(&request)?;
    let mut buffer = [0u8; 4096];
    loop {
        let count = connection.read(&mut buffer)?;
        if count == 0 {
            return Err("authorization session ended without a result".into());
        }
        if let Some(end) = buffer[..count].iter().position(|b| *b == 0) {
            std::io::stdout().write_all(&buffer[..end])?;
            let mut status = buffer[end + 1..count].to_vec();
            connection.read_to_end(&mut status)?;
            if status == b"0" {
                return Ok(());
            }
            return Err(format!(
                "authorized network operation failed ({})",
                String::from_utf8_lossy(&status)
            )
            .into());
        }
        std::io::stdout().write_all(&buffer[..count])?;
        std::io::stdout().flush()?;
    }
}
pub fn session() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    if !cfg!(target_os = "macos") || unsafe { libc::geteuid() } != 0 {
        return Err("requires administrator authorization".into());
    }
    let uid: u32 = std::env::var("SUDO_UID")?.parse()?;
    let gid: u32 = std::env::var("SUDO_GID")?.parse()?;
    if uid == 0 {
        return Err("requires an ordinary user".into());
    }
    let path = socket(uid)?;
    let lock = fs::File::options()
        .create(true)
        .append(true)
        .open(path.with_extension("lock"))?;
    // SAFETY: root-owned lock in /var/run; held until publication completes.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if UnixStream::connect(&path).is_ok() {
        return Ok(());
    }
    let _ = fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
    let name = std::ffi::CString::new(path.as_os_str().as_encoded_bytes())?;
    // SAFETY: root-controlled pathname, valid NUL-terminated buffer.
    if unsafe { libc::lchown(name.as_ptr(), uid, gid) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    listener.set_nonblocking(true)?;
    // Freeze the authorized executable once. Later requests must never
    // re-execute a pathname the requesting user can replace. The networking
    // executable imports only Apple system libraries, checked at assembly.
    let mut template = b"/tmp/clankerd-authorization.XXXXXX\0".to_vec();
    let directory = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
    if directory.is_null() {
        return Err(std::io::Error::last_os_error().into());
    }
    let snapshot = AuthorizedCode(PathBuf::from(
        unsafe { std::ffi::CStr::from_ptr(directory) }.to_str()?,
    ));
    let executable = snapshot.0.join("clankerd-network");
    fs::copy(std::env::current_exe()?, &executable)?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o500))?;
    // No threads exist yet; the child starts a new runtime session. The
    // authorization caller returns once the socket has already been bound.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if pid > 0 {
        // The forked session owns cleanup; the authorizing parent exits now.
        std::mem::forget(snapshot);
        return Ok(());
    }
    unsafe {
        libc::setsid();
    }
    drop(lock);
    // Detach the session from the CLI's log, terminal and sudo monitor.
    let null = fs::File::options()
        .read(true)
        .write(true)
        .open("/dev/null")?;
    for fd in 0..3 {
        unsafe {
            libc::dup2(null.as_raw_fd(), fd);
        }
    }
    let active = Arc::new(AtomicUsize::new(0));
    let mut last = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                last = Instant::now();
                let active = active.clone();
                active.fetch_add(1, Ordering::SeqCst);
                let executable = executable.clone();
                std::thread::spawn(move || {
                    if let Err(error) = serve(stream, &executable, uid, gid) {
                        eprintln!("authorized networking: {error}");
                    }
                    active.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error.into()),
        }
        if active.load(Ordering::SeqCst) > 0 {
            last = Instant::now();
        }
        if last.elapsed() > Duration::from_secs(30) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = fs::remove_file(path);
    Ok(())
}
fn serve(mut stream: UnixStream, executable: &std::path::Path, uid: u32, gid: u32) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut bytes = Vec::new();
    loop {
        let mut byte = [0u8];
        stream.read_exact(&mut byte)?;
        if byte[0] == b'\n' {
            break;
        }
        if bytes.len() >= 65536 {
            return Err("authorization request too large".into());
        }
        bytes.push(byte[0]);
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    if request
        .args
        .iter()
        .any(|arg| ["launch", "authorization-session"].contains(&arg.as_str()))
    {
        return Err("invalid authorized operation".into());
    }
    let output = fs::File::from(std::os::fd::OwnedFd::from(stream.try_clone()?));
    let mut child = Command::new(executable)
        .args(request.args)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("SUDO_UID", uid.to_string())
        .env("SUDO_GID", gid.to_string())
        .stdin(Stdio::piped())
        .stdout(output.try_clone()?)
        .stderr(output)
        .process_group(0)
        .spawn()?;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.input.as_bytes())?;
    loop {
        if let Some(status) = child.try_wait()? {
            stream.write_all(format!("\0{}", status.code().unwrap_or(1)).as_bytes())?;
            return Ok(());
        }
        let mut byte = 0u8;
        // SAFETY: valid one-byte buffer and live stream fd. No incoming data
        // is expected after the request; EOF means its owner disappeared.
        let read = unsafe {
            libc::recv(
                stream.as_raw_fd(),
                (&mut byte as *mut u8).cast(),
                1,
                libc::MSG_PEEK | libc::MSG_DONTWAIT,
            )
        };
        let disconnected = read >= 0
            || !matches!(
                std::io::Error::last_os_error().kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            );
        if disconnected {
            // Let the native loop stop its interface before forcing teardown.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_secs(3);
            while Instant::now() < deadline {
                if child.try_wait()?.is_some() {
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
