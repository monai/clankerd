use std::process::Command;

#[test]
fn version_flag_prints_name_and_semver() {
    let out = Command::new(env!("CARGO_BIN_EXE_vmctl"))
        .arg("--version")
        .output()
        .expect("run vmctl");
    assert!(out.status.success(), "exit status: {:?}", out.status);
    let stdout = String::from_utf8(out.stdout).unwrap();
    let version = stdout
        .trim()
        .strip_prefix("vmctl ")
        .unwrap_or_else(|| panic!("unexpected output: {stdout:?}"));
    let parts: Vec<&str> = version.split('.').collect();
    assert_eq!(parts.len(), 3, "not semver: {version:?}");
    assert!(
        parts.iter().all(|p| p.parse::<u32>().is_ok()),
        "not semver: {version:?}"
    );
}
