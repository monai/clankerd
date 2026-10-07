//! Signal names, as in `docker kill -s` and an image's `StopSignal`.

/// `KILL`, `SIGKILL`, `kill` or `9` (Linux signal numbers 1 to 64).
pub fn parse_signal(s: &str) -> Result<i32, String> {
    if let Ok(n) = s.parse::<i32>() {
        return if (1..=64).contains(&n) {
            Ok(n)
        } else {
            Err(format!("invalid signal number {n}"))
        };
    }
    let name = s.to_ascii_uppercase();
    let name = name.strip_prefix("SIG").unwrap_or(&name);
    Ok(match name {
        "HUP" => libc::SIGHUP,
        "INT" => libc::SIGINT,
        "QUIT" => libc::SIGQUIT,
        "KILL" => libc::SIGKILL,
        "USR1" => libc::SIGUSR1,
        "USR2" => libc::SIGUSR2,
        "TERM" => libc::SIGTERM,
        "CONT" => libc::SIGCONT,
        "STOP" => libc::SIGSTOP,
        "WINCH" => libc::SIGWINCH,
        "ALRM" => libc::SIGALRM,
        "PIPE" => libc::SIGPIPE,
        _ => return Err(format!("unknown signal \"{s}\"")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_numbers_and_prefixes() {
        assert_eq!(parse_signal("KILL"), Ok(9));
        assert_eq!(parse_signal("sigterm"), Ok(15));
        assert_eq!(parse_signal("10"), Ok(10));
        assert!(parse_signal("0").is_err());
        assert!(parse_signal("NOPE").is_err());
    }
}
