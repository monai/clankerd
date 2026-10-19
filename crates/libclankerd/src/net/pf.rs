//! Privileged PF ownership is isolated in the bundled network executable.
use std::io::Write;
use std::process::{Command, Stdio};

use super::NetSpec;
use crate::{Error, NetworkMode, Result};

fn enabled(spec: &NetSpec) -> bool {
    cfg!(target_os = "macos")
        && spec.host_config.network_mode != NetworkMode::Bridged
        && std::env::var("CLANKERD_NETWORK_PF").as_deref() != Ok("off")
}

pub(super) fn register(spec: &NetSpec, pid: u32, prefix: Option<std::net::Ipv6Addr>) -> Result<()> {
    if !enabled(spec) {
        return Ok(());
    }
    let network_id = spec.network_id.as_deref().ok_or_else(|| {
        Error::unavailable("native PF requires a persisted virtual-network identity")
    })?;
    invoke(serde_json::json!({
        "action": "register", "socket": spec.socket, "pid": pid, "network_id": network_id,
        "subnet": format!("{}/{}", spec.host_config.shared_network.address, spec.host_config.shared_network.prefix_len),
        "prefix": prefix,
    }))
}

pub(super) fn remove(spec: &NetSpec) {
    if enabled(spec)
        && let Err(error) = invoke(serde_json::json!({"action": "remove", "socket": spec.socket}))
    {
        eprintln!("clankerd: native PF cleanup: {error}");
    }
}

fn invoke(request: serde_json::Value) -> Result<()> {
    let executable = std::env::current_exe()?;
    let controller = executable
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join("clankerd-network");
    let mut child = Command::new(controller)
        .args(["launch", "pf"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Error::unavailable(format!("starting native PF controller: {error}")))?;
    let written = child
        .stdin
        .take()
        .unwrap()
        .write_all(request.to_string().as_bytes());
    let output = child.wait_with_output()?;
    written?;
    if !output.status.success() {
        // The authorization session forwards the privileged child's stdout and
        // stderr over one stream, which the launcher writes to stdout.
        let diagnostic = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return Err(Error::unavailable(format!(
            "cannot apply native network policy: {}",
            diagnostic.trim()
        )));
    }
    Ok(())
}
