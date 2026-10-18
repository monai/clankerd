mod authorization;
mod pf;
fn main() {
    if let Err(error) = run() {
        eprintln!("clankerd-network: {error}");
        std::process::exit(1);
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>> {
    match std::env::args().nth(1).as_deref() {
        Some("authorize") => return authorization::authorize(),
        Some("launch") => return authorization::launch(std::env::args().skip(2).collect()),
        Some("authorization-session") => return authorization::session(),
        _ => {}
    }

    if std::env::args().nth(1).as_deref() == Some("pf") {
        return pf::run();
    }
    #[cfg(target_os = "macos")]
    {
        use std::ffi::CString;
        unsafe extern "C" {
            fn clankerd_vmnet(argc: i32, argv: *const *const libc::c_char) -> i32;
        }
        let args: Vec<_> = std::env::args()
            .map(CString::new)
            .collect::<Result<_, _>>()?;
        let pointers: Vec<_> = args.iter().map(|s| s.as_ptr()).collect();
        // SAFETY: NUL-terminated argv storage remains live through the native loop.
        let result = unsafe { clankerd_vmnet(pointers.len() as i32, pointers.as_ptr()) };
        if result != 0 {
            return Err(format!("vmnet attachment failed ({result})").into());
        }
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    Err("native networking requires macOS".into())
}
