//! one persistent workload mount namespace; shared guest PID/network namespaces.
use std::ffi::CString;
use std::fs::{self, File};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

static KEEPER: AtomicU32 = AtomicU32::new(0);

/// Interpret absolute image symlinks relative to its root, before traversing
/// them with management's filesystem context. No workload is running while
/// boot prepares these directories.
pub fn resolve_under(
    root: &std::path::Path,
    path: &std::path::Path,
    follow_leaf: bool,
) -> io::Result<PathBuf> {
    use std::{collections::VecDeque, path::Component};
    let mut pending: VecDeque<_> = path
        .components()
        .map(|part| part.as_os_str().to_os_string())
        .collect();
    let mut relative = PathBuf::new();
    let mut links = 0;
    while let Some(part) = pending.pop_front() {
        match std::path::Path::new(&part).components().next() {
            Some(Component::RootDir) => relative.clear(),
            Some(Component::ParentDir) => {
                relative.pop();
            }
            Some(Component::Normal(name)) => {
                relative.push(name);
                let candidate = root.join(&relative);
                match fs::symlink_metadata(&candidate) {
                    Ok(metadata)
                        if metadata.file_type().is_symlink()
                            && (follow_leaf || !pending.is_empty()) =>
                    {
                        links += 1;
                        if links > 40 {
                            return Err(io::Error::from_raw_os_error(libc::ELOOP));
                        }
                        relative.pop();
                        let target = fs::read_link(candidate)?;
                        for component in target.components().rev() {
                            pending.push_front(component.as_os_str().to_os_string());
                        }
                    }
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
            }
            _ => {}
        }
    }
    Ok(root.join(relative))
}
pub fn workload_path(path: &str) -> io::Result<PathBuf> {
    resolve_under(
        std::path::Path::new("/workload"),
        std::path::Path::new(path),
        true,
    )
}
pub fn filesystem_path(path: &std::path::Path) -> io::Result<PathBuf> {
    let keeper = KEEPER.load(Ordering::Relaxed);
    if keeper == 0 {
        return Ok(path.to_path_buf());
    }
    resolve_under(&PathBuf::from(format!("/proc/{keeper}/root")), path, true)
}
pub fn account_path(name: &str) -> io::Result<PathBuf> {
    filesystem_path(&PathBuf::from("/etc").join(name))
}

/// Runtime files belong to this boot, independently of the persistent image
/// and management's own runtime directory.
pub fn prepare_runtime() -> io::Result<()> {
    let run = workload_path("/run")?;
    fs::create_dir_all(&run)?;
    crate::boot::mount(
        "tmpfs",
        run.to_str()
            .ok_or_else(|| io::Error::other("invalid runtime mount path"))?,
        "tmpfs",
        libc::MS_NOSUID | libc::MS_NODEV,
        "mode=0755",
    )
}

pub fn start() -> Result<(), String> {
    let run = workload_path("/run").map_err(|e| e.to_string())?;
    fs::create_dir_all(&run).map_err(|e| e.to_string())?;
    let marker = run.join(".clankerd-namespace-ready");
    let _ = fs::remove_file(&marker);
    let mut cmd = Command::new(format!("/{}", clankerd_proto::spawn::BOOT_GUESTD));
    cmd.arg("namespace-keeper")
        .stdin(std::process::Stdio::null());
    let mut child = crate::power::spawn_owned(&mut cmd).map_err(|e| e.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            crate::power::release(child.id());
            return Err(format!("namespace keeper exited: {status}"));
        }
        if fs::read_to_string(&marker).ok().as_deref() == Some(&child.id().to_string()) {
            KEEPER.store(child.id(), Ordering::Relaxed);
            // PID1's generic reaper now owns this long-lived namespace process.
            crate::power::release(child.id());
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = child.kill();
    let _ = child.wait();
    crate::power::release(child.id());
    Err("namespace readiness timed out".into())
}

pub fn keeper() -> io::Result<()> {
    // This is a fresh executable, so Rust allocation here is safe.
    if unsafe { libc::unshare(libc::CLONE_NEWNS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    crate::boot::mount("", "/", "", libc::MS_PRIVATE | libc::MS_REC, "")?;
    crate::boot::mount(
        "/workload",
        "/workload",
        "",
        libc::MS_BIND | libc::MS_REC,
        "",
    )?;
    for name in ["proc", "sys", "dev", "run/clankerd-resolver"] {
        let target = workload_path(name)?;
        fs::create_dir_all(&target)?;
        crate::boot::mount(
            &format!("/{name}"),
            target
                .to_str()
                .ok_or_else(|| io::Error::other("invalid mount path"))?,
            "",
            libc::MS_BIND | libc::MS_REC,
            "",
        )?;
    }
    fs::create_dir_all(workload_path("/etc")?)?;
    let resolver = resolve_under(
        std::path::Path::new("/workload"),
        std::path::Path::new("/etc/resolv.conf"),
        false,
    )?;
    if fs::symlink_metadata(&resolver).is_ok() {
        fs::remove_file(&resolver)?;
    }
    std::os::unix::fs::symlink("/run/clankerd-resolver/etc/resolv.conf", resolver)?;
    let old_path = workload_path("/.old-management-root")?;
    fs::create_dir_all(&old_path)?;
    let new = c"/workload";
    use std::os::unix::ffi::OsStrExt;
    let old = CString::new(old_path.as_os_str().as_bytes())?;
    let old_in_new = PathBuf::from("/").join(
        old_path
            .strip_prefix("/workload")
            .map_err(io::Error::other)?,
    );
    let old_in_new_c = CString::new(old_in_new.as_os_str().as_bytes())?;
    if unsafe { libc::syscall(libc::SYS_pivot_root, new.as_ptr(), old.as_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }
    std::env::set_current_dir("/")?;
    if unsafe { libc::umount2(old_in_new_c.as_ptr(), libc::MNT_DETACH) } < 0 {
        return Err(io::Error::last_os_error());
    }
    fs::remove_dir(old_in_new)?;
    fs::write(
        "/run/.clankerd-namespace-ready",
        std::process::id().to_string(),
    )?;
    loop {
        unsafe {
            libc::pause();
        }
    }
}

pub fn configure(cmd: &mut Command, cwd: &str) -> io::Result<()> {
    let keeper = KEEPER.load(Ordering::Relaxed);
    if keeper == 0 {
        if !cwd.is_empty() {
            cmd.current_dir(cwd);
        }
        return Ok(());
    }
    // Open before fork; OwnedFd/File are CLOEXEC. No old root directory is
    // retained in the executed workload. stdio is deliberately inherited.
    let namespace = File::open(format!("/proc/{keeper}/ns/mnt"))?;
    let root = File::open(format!("/proc/{keeper}/root"))?;
    let cwd = CString::new(if cwd.is_empty() { "/" } else { cwd })?;
    unsafe {
        cmd.pre_exec(move || {
            if libc::setns(namespace.as_raw_fd(), libc::CLONE_NEWNS) < 0
                || libc::fchdir(root.as_raw_fd()) < 0
                || libc::chroot(c".".as_ptr()) < 0
                || libc::chdir(cwd.as_ptr()) < 0
            {
                return Err(io::Error::last_os_error());
            }
            // Keep Rust's exec-error pipe alive until exec; mark all extra
            // descriptors CLOEXEC rather than closing that pipe prematurely.
            if libc::syscall(
                libc::SYS_close_range,
                3u32,
                u32::MAX,
                libc::CLOSE_RANGE_CLOEXEC,
            ) < 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(())
}
