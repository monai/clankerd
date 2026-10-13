use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// vmnet's own list is the authority for supported physical attachments.
pub(super) fn validate_interface(binary: &Path, interface: &str) -> Result<()> {
    let mut command = if cfg!(target_os = "macos") {
        let mut command = Command::new("sudo");
        command.args(["--non-interactive", "--"]).arg(binary);
        command
    } else {
        Command::new(binary)
    };
    let output = command
        .arg("--list-shared-interfaces")
        .output()
        .map_err(|e| Error::unavailable(format!("cannot list vmnet physical interfaces: {e}")))?;
    if !output.status.success() {
        return Err(Error::unavailable(format!(
            "cannot list vmnet physical interfaces: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let supported = String::from_utf8_lossy(&output.stdout);
    if !supported.lines().any(|line| line.trim() == interface) {
        return Err(Error::unavailable(format!(
            "bridge interface {interface} is unavailable or unsupported by vmnet; supported physical interfaces: {}",
            supported.split_whitespace().collect::<Vec<_>>().join(", ")
        )));
    }
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/sbin/ifconfig")
            .arg(interface)
            .output()
            .map_err(|e| {
                Error::unavailable(format!("cannot inspect bridge interface {interface}: {e}"))
            })?;
        let status = String::from_utf8_lossy(&output.stdout);
        let flags = status.lines().next().unwrap_or_default();
        if !output.status.success()
            || !flags.split(['<', '>', ',']).any(|flag| flag == "UP")
            || !status.lines().any(|line| line.trim() == "status: active")
        {
            return Err(Error::unavailable(format!(
                "selected bridge interface {interface} is not active; connect or enable this physical interface before starting the machine"
            )));
        }
    }
    Ok(())
}
