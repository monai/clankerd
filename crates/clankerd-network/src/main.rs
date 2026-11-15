#[cfg(target_os = "macos")]
mod daemon;
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod egress;
#[cfg(target_os = "macos")]
mod interface;
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod packets;
#[cfg(any(target_os = "macos", test))]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod pf;
fn main() {
    #[cfg(target_os = "macos")]
    {
        if unsafe { libc::geteuid() } != 0 {
            eprintln!("clankerd-network must be started by its system LaunchDaemon");
            std::process::exit(1);
        }
        daemon::run();
    }
    #[cfg(not(target_os = "macos"))]
    {
        eprintln!("clankerd-network requires macOS");
        std::process::exit(1);
    }
}

#[cfg(any(target_os = "macos", test))]
fn cleanup_failed(error: impl std::fmt::Display) -> ! {
    eprintln!(
        "clankerd-network: required cleanup failed: {error}; terminating helper; restart machines manually after helper recovery"
    );
    std::process::exit(1);
}

#[cfg(test)]
mod tests {
    #[test]
    fn cleanup_failure_terminates_before_later_success() {
        const CHILD: &str = "CLANKERD_TEST_CLEANUP_FAILURE";
        if std::env::var_os(CHILD).is_some() {
            super::cleanup_failed("injected release failure");
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tests::cleanup_failure_terminates_before_later_success",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1));
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        assert!(diagnostic.contains("injected release failure"));
        assert!(diagnostic.contains("restart machines manually"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("test result: ok"));
    }
}
