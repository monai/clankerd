use std::{env, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=src/vmnet.c");
    println!("cargo:rerun-if-changed=src/vmnet-abi.h");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let object = out.join("vmnet.o");
    let mut cc = if env::var("HOST").unwrap().contains("apple-darwin") {
        let mut command = Command::new("xcrun");
        command.arg("clang");
        command
    } else {
        let mut command = Command::new("zig");
        command.args(["cc", "-target", "aarch64-macos", "-DCLANKERD_CROSS=1"]);
        command
    };
    assert!(
        cc.args([
            "-fblocks",
            "-O2",
            "-mmacosx-version-min=15.0",
            "-c",
            "src/vmnet.c",
            "-o"
        ])
        .arg(&object)
        .status()
        .unwrap()
        .success()
    );
    assert!(
        Command::new("ar")
            .arg("crs")
            .arg(out.join("libclankerd_vmnet.a"))
            .arg(object)
            .status()
            .unwrap()
            .success()
    );
    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=clankerd_vmnet");
    println!("cargo:rustc-link-lib=framework=vmnet");
}
