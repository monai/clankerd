use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Cargo is the source of truth for executable paths, including instrumented builds.
pub fn build_binary(package: &str) -> PathBuf {
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.args([
        "build",
        "--message-format=json",
        "-p",
        package,
        "--bin",
        package,
    ]);
    if !cfg!(debug_assertions) {
        command.arg("--release");
    }
    let output = command
        .stderr(Stdio::inherit())
        .output()
        .expect("run cargo build");
    assert!(output.status.success(), "building {package} failed");
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|message| {
            if message["reason"] == "compiler-artifact" && message["target"]["name"] == package {
                message["executable"].as_str().map(PathBuf::from)
            } else {
                None
            }
        })
        .unwrap_or_else(|| panic!("cargo reported no executable for {package}"))
}
