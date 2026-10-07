//! The guest-side block list: destinations inside the network backend's
//! virtual network that neither guestd's children nor containers behind the
//! guest may reach (gvproxy's in-guest control API and its host-loopback
//! alias).
//!
//! The rules go in their own nftables table, hooked into `output` (processes
//! in the guest) and `forward` (containers, whose traffic is routed through
//! the guest) at a priority before Docker's own tables, so Docker's nftables
//! mode neither removes nor overrides them. guestd runs the `nft` binary the
//! image ships for Docker (the image installs the nftables package); with no
//! `nft` the caller must not bring the network up.
//!
//! Limit: this is a guest-kernel firewall. Root inside the guest with
//! CAP_NET_ADMIN in the initial network namespace (a privileged container, or
//! `--network host`) can delete it. gvproxy has no flag that closes the
//! endpoint from its side.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use clankerd_proto::guest::BlockedEndpoint;

const TABLE: &str = "clankerd_guard";

/// The nft script that installs the block list (idempotent: it replaces its own table).
pub fn ruleset(blocked: &[BlockedEndpoint]) -> String {
    let rules: String = blocked
        .iter()
        .map(|b| match b.tcp_port {
            Some(port) => format!("\t\tip daddr {} tcp dport {port} drop\n", b.addr),
            None => format!("\t\tip daddr {} drop\n", b.addr),
        })
        .collect();
    let chain = |name: &str| {
        format!(
            "\tchain {name} {{\n\t\ttype filter hook {name} priority -10; policy accept;\n{rules}\t}}\n"
        )
    };
    format!(
        "add table inet {TABLE}\ndelete table inet {TABLE}\ntable inet {TABLE} {{\n{}{}}}\n",
        chain("forward"),
        chain("output")
    )
}

fn nft_binary() -> Option<PathBuf> {
    ["/usr/sbin/nft", "/sbin/nft", "/usr/bin/nft", "/bin/nft"]
        .into_iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

/// Installs the block list. Fails when `nft` is missing or rejects the rules.
pub fn apply(blocked: &[BlockedEndpoint]) -> Result<(), String> {
    if blocked.is_empty() {
        return Ok(());
    }
    let nft = nft_binary().ok_or(
        "nft not found in the image (install the nftables package): \
         cannot block the network backend's control endpoints",
    )?;
    let mut child = Command::new(&nft)
        .args(["-f", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("running {}: {e}", nft.display()))?;
    child
        .stdin
        .take()
        .expect("stdin was piped")
        .write_all(ruleset(blocked).as_bytes())
        .map_err(|e| format!("writing the ruleset to nft: {e}"))?;
    let out = child
        .wait_with_output()
        .map_err(|e| format!("waiting for nft: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "nft rejected the block list: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    fn gvproxy_blocklist() -> Vec<BlockedEndpoint> {
        vec![
            BlockedEndpoint {
                addr: Ipv4Addr::new(192, 168, 127, 1),
                tcp_port: Some(80),
            },
            BlockedEndpoint {
                addr: Ipv4Addr::new(192, 168, 127, 254),
                tcp_port: None,
            },
        ]
    }

    #[test]
    fn the_ruleset_drops_the_control_api_and_the_loopback_alias_on_both_paths() {
        let expected = "\
add table inet clankerd_guard
delete table inet clankerd_guard
table inet clankerd_guard {
\tchain forward {
\t\ttype filter hook forward priority -10; policy accept;
\t\tip daddr 192.168.127.1 tcp dport 80 drop
\t\tip daddr 192.168.127.254 drop
\t}
\tchain output {
\t\ttype filter hook output priority -10; policy accept;
\t\tip daddr 192.168.127.1 tcp dport 80 drop
\t\tip daddr 192.168.127.254 drop
\t}
}
";
        assert_eq!(ruleset(&gvproxy_blocklist()), expected);
    }

    /// `nft -c` parses before it talks to the kernel, so without privileges it
    /// still fails on a syntax error and only on that.
    #[test]
    fn nft_accepts_the_syntax_of_the_generated_ruleset() {
        let Some(nft) = nft_binary() else {
            eprintln!("nft not installed: skipping the syntax check");
            return;
        };
        let mut child = Command::new(nft)
            .args(["-c", "-f", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(ruleset(&gvproxy_blocklist()).as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            !stderr.contains("syntax error")
                && !stderr.contains("Error: unknown")
                && !stderr.contains("Error: No such"),
            "nft rejected the ruleset: {stderr}"
        );
    }

    #[test]
    fn nothing_to_block_means_nothing_to_run() {
        assert_eq!(apply(&[]), Ok(()));
    }
}
