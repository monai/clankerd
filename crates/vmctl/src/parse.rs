//! Parsers for CLI values (`-v`, `-p`).

use std::path::PathBuf;

use libclankerd::{Mount, PortBinding};

/// Parses `SOURCE:TARGET[:OPTIONS]` where OPTIONS are comma-separated `ro` and
/// `size=N`. A source starting with `.`, `/` or `~` is a host directory (made
/// absolute against the current directory); anything else is a volume name.
pub fn parse_mount(s: &str) -> Result<Mount, String> {
    let parts: Vec<&str> = s.split(':').collect();
    let (source, target, options) = match parts.as_slice() {
        [source, target] => (*source, *target, ""),
        [source, target, options] => (*source, *target, *options),
        _ => return Err("expected SOURCE:TARGET[:OPTIONS]".into()),
    };
    if source.is_empty() || !target.starts_with('/') {
        return Err("expected SOURCE:/ABSOLUTE/TARGET[:OPTIONS]".into());
    }
    let (mut read_only, mut size) = (false, None);
    for opt in options.split(',').filter(|o| !o.is_empty()) {
        match opt.split_once('=') {
            None if opt == "ro" => read_only = true,
            None if opt == "rw" => read_only = false,
            Some(("size", v)) => {
                size = Some(libclankerd::parse_size(v).map_err(|e| e.message().to_owned())?)
            }
            _ => return Err(format!("unknown mount option \"{opt}\" (use ro or size=N)")),
        }
    }
    if source.starts_with(['.', '/', '~']) {
        if size.is_some() {
            return Err("size= only applies to named volumes".into());
        }
        let expanded = match source.strip_prefix("~/") {
            Some(rest) => PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(rest),
            None => PathBuf::from(source),
        };
        let absolute = std::path::absolute(&expanded).map_err(|e| e.to_string())?;
        // Resolve `..` and symlinks when the directory exists; the library
        // reports a missing one.
        let source = absolute.canonicalize().unwrap_or(absolute);
        return Ok(Mount::Bind {
            source,
            target: target.into(),
            read_only,
        });
    }
    if read_only {
        return Err("ro only applies to host directories".into());
    }
    let mount = Mount::volume(source, target);
    Ok(match size {
        Some(bytes) => mount.with_size(bytes),
        None => mount,
    })
}

/// Parses `[IP:]HOST_PORT:GUEST_PORT`. The library rejects non-loopback IPs.
pub fn parse_publish(s: &str) -> Result<PortBinding, String> {
    let port = |p: &str| {
        p.parse::<u16>()
            .map_err(|_| format!("invalid port \"{p}\""))
    };
    let parts: Vec<&str> = s.rsplitn(3, ':').collect();
    match parts.as_slice() {
        [guest, host] => Ok(PortBinding::loopback(port(host)?, port(guest)?)),
        [guest, host, ip] => Ok(PortBinding {
            host_ip: Some(ip.parse().map_err(|_| format!("invalid IP \"{ip}\""))?),
            host_port: port(host)?,
            guest_port: port(guest)?,
        }),
        _ => Err("expected [IP:]HOST_PORT:GUEST_PORT".into()),
    }
}
