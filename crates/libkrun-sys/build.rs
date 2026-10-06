//! Links Homebrew's libkrun (1.19.x) on macOS.
//!
//! The link goes against `stubs/libkrun.tbd`, a text stub listing the symbols
//! declared in `src/ffi.rs` with the install name
//! `/opt/homebrew/lib/libkrun.1.dylib`. That makes the same binary come out
//! whether it is linked on the Mac (with or without libkrun installed) or
//! cross-linked from Linux with zig, which has no macOS SDK and cannot read
//! the real dylib. At run time dyld loads the real library from Homebrew's
//! prefix. Other targets link nothing.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=stubs/libkrun.tbd");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let stubs = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("stubs");
        println!("cargo:rustc-link-search=native={}", stubs.display());
        println!("cargo:rustc-link-lib=dylib=krun");
    }
}
