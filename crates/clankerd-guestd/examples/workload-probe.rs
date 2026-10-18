use std::{fs, thread, time::Duration};
fn read(p: &str) -> String {
    fs::read_to_string(p).unwrap_or_default()
}
fn main() {
    match std::env::args().nth(1).as_deref() {
        Some("sleep") => loop {
            thread::sleep(Duration::from_secs(1));
        },
        Some("mount") => {
            fs::create_dir_all("/mount-check").unwrap();
            assert_eq!(
                unsafe {
                    libc::mount(
                        c"tmpfs".as_ptr(),
                        c"/mount-check".as_ptr(),
                        c"tmpfs".as_ptr(),
                        0,
                        std::ptr::null(),
                    )
                },
                0,
                "{}",
                std::io::Error::last_os_error()
            );
            fs::write("/mount-check/private", "workload mount").unwrap();
            assert!(
                !std::path::Path::new("/proc/1/root/workload/mount-check/private").exists(),
                "mount propagated into management namespace"
            );
            println!("PASS private propagation with unrestricted CAP_SYS_ADMIN");
        }
        Some("mount-visible") => assert_eq!(read("/mount-check/private"), "workload mount"),
        _ => {
            let status = read("/proc/self/status");
            let fds: Vec<_> = fs::read_dir("/proc/self/fd")
                .unwrap()
                .flatten()
                .filter_map(|e| {
                    fs::read_link(e.path())
                        .ok()
                        .map(|p| p.display().to_string())
                })
                .collect();
            let value = serde_json::json!({
                "pid":std::process::id(), "cwd":std::env::current_dir().unwrap(),
                "management_pid1_exe":fs::read_link("/proc/1/exe").unwrap(),
                "same_network_namespace":fs::read_link("/proc/self/ns/net").unwrap()==fs::read_link("/proc/1/ns/net").unwrap(),
                "different_mount_namespace":fs::read_link("/proc/self/ns/mnt").unwrap()!=fs::read_link("/proc/1/ns/mnt").unwrap(),
                "workload_passwd":read("/etc/passwd"),"management_passwd":read("/proc/1/root/etc/passwd"),
                "resolver":read("/etc/resolv.conf"),"shell_absent":!std::path::Path::new("/bin/sh").exists(),
                "status":status,"fds":fds,"mountinfo":read("/proc/self/mountinfo"),"management_mountinfo":read("/proc/1/mountinfo"),
                "memory":read("/proc/meminfo"),"pid1_status":read("/proc/1/status"),
                "kernel":read("/proc/version"),"filesystems":read("/proc/filesystems"),
                "management_resolver":read("/proc/1/root/etc/resolv.conf"),
                "hook_events":read("/proc/1/root/run/clankerd/network/hook-events.jsonl"),
                "processes":fs::read_dir("/proc").unwrap().flatten().filter_map(|e|{
                    let id=e.file_name().to_str()?.parse::<u32>().ok()?;
                    let base=format!("/proc/{id}");
                    Some(serde_json::json!({"pid":id,"status":read(&format!("{base}/status")),"cmdline":read(&format!("{base}/cmdline")).replace('\0'," "),"root":fs::read_link(format!("{base}/root")).ok(),"exe":fs::read_link(format!("{base}/exe")).ok()}))
                }).collect::<Vec<_>>()
            });
            println!("{value}");
        }
    }
}
