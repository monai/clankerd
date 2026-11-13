#[cfg(target_os = "macos")]
use std::process::Command;

use crate::error::{Error, Result};

/// vmnet's own list is the authority for supported physical attachments.
pub(super) fn validate_interface(interface: &str, supported: &[String]) -> Result<()> {
    if !supported.iter().any(|name| name == interface) {
        return Err(Error::unavailable(format!(
            "bridge interface {interface} is unavailable or unsupported by vmnet; supported physical interfaces: {}",
            supported.join(", ")
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
