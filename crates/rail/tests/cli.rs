use std::process::Command;

#[test]
fn version_flag_prints_name_and_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_rail"))
        .arg("--version")
        .output()
        .expect("run rail");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("rail 0.1.0"), "got {stdout:?}");
}

#[test]
fn no_args_explains_tui_is_not_built_yet() {
    let out = Command::new(env!("CARGO_BIN_EXE_rail"))
        .output()
        .expect("run rail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not built yet"), "got {stderr:?}");
}
