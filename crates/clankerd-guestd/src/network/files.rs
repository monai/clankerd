//! Files and sysctls guestd writes for the network. Every function takes the
//! root it works under, so tests run against temp directories and `boot`
//! passes `/` and `/proc/sys`.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::Ipv4Addr;
use std::path::Path;

/// Docker needs IPv4 forwarding to route container traffic out through eth0.
pub const FORWARDING_SYSCTLS: [(&str, &str); 3] = [
    ("net.ipv4.ip_forward", "1"),
    ("net.ipv6.conf.all.forwarding", "1"),
    ("net.ipv6.conf.default.forwarding", "1"),
];

/// Writes each `(dotted.key, value)` under `proc_sys` (`/proc/sys`). Keys that
/// cannot be set (absent in this kernel, read-only) are returned as messages;
/// the rest are still written.
pub fn write_sysctls(proc_sys: &Path, entries: &[(&str, &str)]) -> Vec<String> {
    let mut errors = Vec::new();
    for (key, value) in entries {
        let path = proc_sys.join(key.replace('.', "/"));
        // No `create`: /proc/sys has no files for keys the kernel lacks.
        let result = OpenOptions::new()
            .write(true)
            .open(&path)
            .and_then(|mut f| f.write_all(value.as_bytes()));
        if let Err(e) = result {
            errors.push(format!("sysctl {key}={value}: {e}"));
        }
    }
    errors
}

/// Writes `/etc/resolv.conf` under `root`, replacing a symlink if the image
/// ships one (systemd-resolved stubs point at files that do not exist here).
pub fn write_resolv_conf(
    root: &Path,
    nameservers: &[Ipv4Addr],
    search: &[String],
) -> io::Result<()> {
    let mut text = String::new();
    for ns in nameservers {
        text.push_str(&format!("nameserver {ns}\n"));
    }
    if !search.is_empty() {
        text.push_str(&format!("search {}\n", search.join(" ")));
    }
    let etc = root.join("etc");
    fs::create_dir_all(&etc)?;
    let path = etc.join("resolv.conf");
    if fs::symlink_metadata(&path).is_ok_and(|m| m.file_type().is_symlink()) {
        fs::remove_file(&path)?;
    }
    fs::write(path, text)
}

/// Writes `/etc/hostname` and makes sure `/etc/hosts` resolves the name.
pub fn write_hostname(root: &Path, name: &str) -> io::Result<()> {
    let etc = root.join("etc");
    fs::create_dir_all(&etc)?;
    fs::write(etc.join("hostname"), format!("{name}\n"))?;
    let hosts_path = etc.join("hosts");
    let mut hosts = fs::read_to_string(&hosts_path).unwrap_or_else(|_| {
        "127.0.0.1\tlocalhost\n::1\tlocalhost ip6-localhost ip6-loopback\n".into()
    });
    let known = hosts
        .lines()
        .any(|l| l.split_whitespace().skip(1).any(|n| n == name));
    if !known {
        if !hosts.ends_with('\n') {
            hosts.push('\n');
        }
        hosts.push_str(&format!("127.0.1.1\t{name}\n"));
    }
    fs::write(hosts_path, hosts)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc_sys() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("net/ipv4")).unwrap();
        fs::create_dir_all(dir.path().join("net/ipv6/conf/all")).unwrap();
        fs::create_dir_all(dir.path().join("net/ipv6/conf/default")).unwrap();
        fs::write(dir.path().join("net/ipv4/ip_forward"), "0\n").unwrap();
        fs::write(dir.path().join("net/ipv6/conf/all/forwarding"), "0\n").unwrap();
        fs::write(dir.path().join("net/ipv6/conf/default/forwarding"), "0\n").unwrap();
        dir
    }

    #[test]
    fn forwarding_sysctls_are_turned_on() {
        let sys = proc_sys();
        let errors = write_sysctls(sys.path(), &FORWARDING_SYSCTLS);
        assert!(errors.is_empty(), "{errors:?}");
        let read = |p: &str| fs::read_to_string(sys.path().join(p)).unwrap();
        assert_eq!(read("net/ipv4/ip_forward").trim(), "1");
        assert_eq!(read("net/ipv6/conf/all/forwarding").trim(), "1");
        assert_eq!(read("net/ipv6/conf/default/forwarding").trim(), "1");
    }

    #[test]
    fn a_sysctl_this_kernel_lacks_is_reported_and_does_not_stop_the_others() {
        let sys = proc_sys();
        fs::remove_file(sys.path().join("net/ipv6/conf/all/forwarding")).unwrap();
        let errors = write_sysctls(sys.path(), &FORWARDING_SYSCTLS);
        assert_eq!(errors.len(), 1, "{errors:?}");
        assert!(
            errors[0].contains("net.ipv6.conf.all.forwarding"),
            "{errors:?}"
        );
        // Not created out of thin air.
        assert!(!sys.path().join("net/ipv6/conf/all/forwarding").exists());
        assert_eq!(
            fs::read_to_string(sys.path().join("net/ipv4/ip_forward"))
                .unwrap()
                .trim(),
            "1"
        );
    }

    #[test]
    fn resolv_conf_lists_the_leased_servers_and_search_domains() {
        let root = tempfile::tempdir().unwrap();
        write_resolv_conf(
            root.path(),
            &[Ipv4Addr::new(192, 168, 127, 1), Ipv4Addr::new(1, 1, 1, 1)],
            &["corp.example".into(), "lan".into()],
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("etc/resolv.conf")).unwrap(),
            "nameserver 192.168.127.1\nnameserver 1.1.1.1\nsearch corp.example lan\n"
        );
    }

    #[test]
    fn resolv_conf_without_search_domains_has_no_search_line() {
        let root = tempfile::tempdir().unwrap();
        write_resolv_conf(root.path(), &[Ipv4Addr::new(10, 0, 0, 1)], &[]).unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("etc/resolv.conf")).unwrap(),
            "nameserver 10.0.0.1\n"
        );
    }

    #[test]
    fn a_dangling_resolv_conf_symlink_is_replaced_by_a_file() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc")).unwrap();
        std::os::unix::fs::symlink(
            "../run/systemd/resolve/stub-resolv.conf",
            root.path().join("etc/resolv.conf"),
        )
        .unwrap();
        write_resolv_conf(root.path(), &[Ipv4Addr::new(10, 0, 0, 1)], &[]).unwrap();
        let meta = fs::symlink_metadata(root.path().join("etc/resolv.conf")).unwrap();
        assert!(meta.is_file());
        assert!(
            !root.path().join("run").exists(),
            "must not write through the link"
        );
    }

    #[test]
    fn hostname_is_written_and_resolvable_through_hosts() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("etc")).unwrap();
        fs::write(
            root.path().join("etc/hosts"),
            "127.0.0.1\tlocalhost\n::1\tlocalhost\n",
        )
        .unwrap();
        write_hostname(root.path(), "0123456789ab").unwrap();
        assert_eq!(
            fs::read_to_string(root.path().join("etc/hostname")).unwrap(),
            "0123456789ab\n"
        );
        let hosts = fs::read_to_string(root.path().join("etc/hosts")).unwrap();
        assert!(hosts.starts_with("127.0.0.1\tlocalhost\n"), "{hosts}");
        assert!(hosts.contains("127.0.1.1\t0123456789ab\n"), "{hosts}");

        // Idempotent.
        write_hostname(root.path(), "0123456789ab").unwrap();
        let again = fs::read_to_string(root.path().join("etc/hosts")).unwrap();
        assert_eq!(again.matches("0123456789ab").count(), 1, "{again}");
    }

    #[test]
    fn hosts_is_created_with_localhost_when_the_image_has_none() {
        let root = tempfile::tempdir().unwrap();
        write_hostname(root.path(), "box").unwrap();
        let hosts = fs::read_to_string(root.path().join("etc/hosts")).unwrap();
        assert!(hosts.contains("127.0.0.1\tlocalhost\n"), "{hosts}");
        assert!(hosts.contains("127.0.1.1\tbox\n"), "{hosts}");
    }
}
