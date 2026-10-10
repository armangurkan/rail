use std::process::Command;

/// A `rail` command whose four XDG-style directories live under `dir`, so no
/// test ever reads or writes the real config, store or secrets.
fn rail_cmd(dir: &tempfile::TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rail"));
    cmd.env("RAIL_CONFIG_DIR", dir.path().join("config"))
        .env("RAIL_DATA_DIR", dir.path().join("data"))
        .env("RAIL_CACHE_DIR", dir.path().join("cache"))
        .env("RAIL_STATE_DIR", dir.path().join("state"));
    cmd
}

#[test]
fn version_flag_prints_name_and_version() {
    let dir = tempfile::tempdir().unwrap();
    let out = rail_cmd(&dir).arg("--version").output().expect("run rail");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.starts_with("rail 0.1.0"), "got {stdout:?}");
}

#[test]
fn no_args_explains_tui_is_not_built_yet() {
    let dir = tempfile::tempdir().unwrap();
    let out = rail_cmd(&dir).output().expect("run rail");
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not built yet"), "got {stderr:?}");
}

#[test]
fn account_list_without_config_explains_how_to_start() {
    let dir = tempfile::tempdir().unwrap();
    let out = rail_cmd(&dir).args(["account", "list"]).output().unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("rail account add"), "got {stderr:?}");
}

#[test]
fn account_add_refuses_duplicate_address_before_opening_a_browser() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_file = config_dir.join("config.toml");
    let original = "[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"x\"\n";
    std::fs::write(&config_file, original).unwrap();

    let out = rail_cmd(&dir)
        .args([
            "account",
            "add",
            "gmail",
            "--address",
            "ME@example.com",
            "--client-id",
            "y",
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("already configured"), "got {stderr:?}");
    assert!(stderr.contains("config.toml"), "got {stderr:?}");
    // Refused before any login: no URL printed, config untouched, no store created.
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("http"), "got {stdout:?}");
    assert_eq!(std::fs::read_to_string(&config_file).unwrap(), original);
    assert!(!dir.path().join("data").exists());
}

#[test]
fn account_add_refuses_duplicate_name_before_opening_a_browser() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_file = config_dir.join("config.toml");
    let original = "[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"x\"\n";
    std::fs::write(&config_file, original).unwrap();

    let out = rail_cmd(&dir)
        .args([
            "account",
            "add",
            "gmail",
            "--address",
            "other@example.com",
            "--client-id",
            "x",
            "--name",
            "work",
        ])
        .output()
        .unwrap();

    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("work"), "got {stderr:?}");
    assert!(
        stderr.to_lowercase().contains("duplicate"),
        "got {stderr:?}"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("http"), "got {stdout:?}");
    assert_eq!(std::fs::read_to_string(&config_file).unwrap(), original);
    assert!(!dir.path().join("data").exists());
}

#[test]
fn doctor_exits_nonzero_without_config() {
    let dir = tempfile::tempdir().unwrap();
    let out = rail_cmd(&dir).arg("doctor").output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("FAIL config"), "got {stdout:?}");
    assert!(stdout.contains("rail account add"), "got {stdout:?}");
    assert!(stdout.contains("no config file at"), "got {stdout:?}");
    assert!(!stdout.contains("ok   secrets"), "got {stdout:?}");
    assert!(stdout.contains("ok   store"), "got {stdout:?}");
    assert!(
        dir.path().join("state").join("rail.log").is_file(),
        "log file created"
    );
}

#[test]
fn account_add_fails_before_the_browser_when_secrets_file_is_corrupt() {
    let dir = tempfile::tempdir().unwrap();
    let config_dir = dir.path().join("config");
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = "[general]\nsecrets = \"file\"\n";
    std::fs::write(config_dir.join("config.toml"), config).unwrap();
    std::fs::write(data_dir.join("secrets.json"), "{not json").unwrap();
    let out = rail_cmd(&dir)
        .args([
            "account",
            "add",
            "gmail",
            "--address",
            "new@example.com",
            "--client-id",
            "x",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("corrupt"), "got {stderr:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("http"), "got {stdout:?}");
    assert_eq!(
        std::fs::read_to_string(config_dir.join("config.toml")).unwrap(),
        config
    );
}

#[test]
fn panic_inside_the_alternate_screen_restores_the_terminal_before_the_message() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("terminal.txt");
    let file = std::fs::File::create(&log).unwrap();
    // stdout and stderr share one file description, so the file keeps their real order.
    let status = rail_cmd(&dir)
        .arg("--panic-test")
        .env("RUST_BACKTRACE", "0")
        .stdout(file.try_clone().unwrap())
        .stderr(file)
        .status()
        .unwrap();
    assert!(!status.success());
    let out = String::from_utf8_lossy(&std::fs::read(&log).unwrap()).into_owned();
    let entered = out
        .find("\x1b[?1049h")
        .expect("entered the alternate screen");
    let left = out.find("\x1b[?1049l").expect("left the alternate screen");
    let message = out.find("rail panic test").expect("panic message printed");
    assert!(entered < left && left < message, "{out:?}");
}
