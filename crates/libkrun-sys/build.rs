//! Link against an SDK-independent stub; the completed artifact owns libkrun.
use std::path::PathBuf;
fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=stubs/libkrun.tbd");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let library = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("stubs");
        println!("cargo:rustc-link-search=native={}", library.display());
        println!("cargo:rustc-link-lib=dylib=krun");
        println!("cargo:rustc-link-arg=-Wl,-rpath,@executable_path/lib");
    }
}
