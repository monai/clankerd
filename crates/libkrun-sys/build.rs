//! Links Homebrew's libkrun (1.19.x) on macOS.
//!
//! Native Mac builds link the installed Homebrew library. Linux cross-builds
//! use a text stub because Zig cannot read the real Mach-O dylib there.
//! Both load Homebrew's libkrun at runtime; other targets link nothing.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=stubs/libkrun.tbd");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let library = if std::env::var("HOST").unwrap().ends_with("-apple-darwin") {
            PathBuf::from("/opt/homebrew/lib")
        } else {
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("stubs")
        };
        println!("cargo:rustc-link-search=native={}", library.display());
        println!("cargo:rustc-link-lib=dylib=krun");
    }
}
