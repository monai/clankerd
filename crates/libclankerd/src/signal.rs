//! Signal names, as in `docker kill -s` and an image's `StopSignal`.

/// `KILL`, `SIGKILL`, `kill` or `9` (Linux guest signal numbers 1 to 64).
/// Names use the Linux ABI regardless of the caller's host platform.
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
        "HUP" => 1,
        "INT" => 2,
        "QUIT" => 3,
        "KILL" => 9,
        "USR1" => 10,
        "USR2" => 12,
        "TERM" => 15,
        "CONT" => 18,
        "STOP" => 19,
        "WINCH" => 28,
        "ALRM" => 14,
        "PIPE" => 13,
        _ => return Err(format!("unknown signal \"{s}\"")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_signals_use_linux_guest_numbers_on_every_host() {
        for (name, expected) in [
            ("HUP", 1),
            ("INT", 2),
            ("QUIT", 3),
            ("KILL", 9),
            ("USR1", 10),
            ("USR2", 12),
            ("TERM", 15),
            ("CONT", 18),
            ("STOP", 19),
            ("WINCH", 28),
            ("ALRM", 14),
            ("PIPE", 13),
        ] {
            assert_eq!(
                parse_signal(name),
                Ok(expected),
                "Linux guest ABI for {name}"
            );
            assert_eq!(
                parse_signal(&format!("sig{}", name.to_ascii_lowercase())),
                Ok(expected)
            );
        }
    }

    #[test]
    fn names_numbers_and_prefixes() {
        assert_eq!(parse_signal("KILL"), Ok(9));
        assert_eq!(parse_signal("sigterm"), Ok(15));
        assert_eq!(parse_signal("10"), Ok(10));
        assert!(parse_signal("0").is_err());
        assert!(parse_signal("NOPE").is_err());
    }
}
