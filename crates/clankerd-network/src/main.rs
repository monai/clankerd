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
