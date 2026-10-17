use std::collections::HashMap;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

/// Cargo is the source of truth for executable paths, including instrumented builds.
pub fn build_binary(package: &str) -> PathBuf {
    if let Some(paths) = std::env::var_os("CLANKERD_TEST_BINARIES") {
        let paths: HashMap<String, PathBuf> = serde_json::from_str(&paths.to_string_lossy())
            .expect("CLANKERD_TEST_BINARIES must be a JSON executable map");
        let path = paths
            .get(package)
            .unwrap_or_else(|| panic!("no prebuilt executable for {package}"));
        assert!(
            path.is_file(),
            "prebuilt executable missing: {}",
            path.display()
        );
        return path.clone();
    }
    static BUILT: OnceLock<Mutex<HashMap<String, PathBuf>>> = OnceLock::new();
    let mut built = BUILT
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap();
    if let Some(path) = built.get(package) {
        return path.clone();
    }
    let mut command = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    command.args([
        "build",
        "--locked",
        "--message-format=json",
        "-p",
        package,
        "--bin",
        package,
    ]);
    let executable = std::env::current_exe().expect("current test executable");
    let profile = executable
        .parent()
        .and_then(|deps| deps.parent())
        .and_then(|directory| directory.file_name())
        .and_then(|name| name.to_str())
        .expect("Cargo test profile directory");
    command.args([
        "--profile",
        if profile == "debug" { "dev" } else { profile },
    ]);
    let output = command
        .stderr(Stdio::inherit())
        .output()
        .expect("run cargo build");
    assert!(output.status.success(), "building {package} failed");
    let path = String::from_utf8(output.stdout)
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
        .unwrap_or_else(|| panic!("cargo reported no executable for {package}"));
    built.insert(package.to_owned(), path.clone());
    path
}
