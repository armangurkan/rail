use std::process::Command;

/// Point a command's four XDG-style directories under `dir`, so no test ever reads or
/// writes the real config, store or secrets.
fn rail_env<'a>(cmd: &'a mut Command, dir: &tempfile::TempDir) -> &'a mut Command {
    cmd.env("RAIL_CONFIG_DIR", dir.path().join("config"))
        .env("RAIL_DATA_DIR", dir.path().join("data"))
        .env("RAIL_CACHE_DIR", dir.path().join("cache"))
        .env("RAIL_STATE_DIR", dir.path().join("state"))
}

/// A `rail` command confined to `dir` by `rail_env`.
fn rail_cmd(dir: &tempfile::TempDir) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_rail"));
    rail_env(&mut cmd, dir);
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
fn rail_with_a_config_says_the_tui_is_not_built_yet() {
    let dir = tempfile::tempdir().unwrap();
    write_config(&dir, "[general]\nsecrets = \"file\"\n");
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

const WORK_FILE_BACKEND: &str = "[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\nclient_secret = \"sec\"\n";

fn write_config(dir: &tempfile::TempDir, text: &str) -> std::path::PathBuf {
    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let file = config_dir.join("config.toml");
    std::fs::write(&file, text).unwrap();
    file
}

/// A browser stand-in: writes the URL it was given to `$RAIL_TEST_URL_FILE` (atomically,
/// so a reader never sees half of it) and exits.
#[cfg(unix)]
fn write_opener(dir: &tempfile::TempDir) -> std::path::PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let path = dir.path().join("opener.sh");
    let script = "#!/bin/sh\nprintf '%s' \"$1\" > \"$RAIL_TEST_URL_FILE.tmp\"\nmv \"$RAIL_TEST_URL_FILE.tmp\" \"$RAIL_TEST_URL_FILE\"\n";
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

fn query(url: &url::Url, key: &str) -> String {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| panic!("{key} missing from {url}"))
}

#[cfg(unix)]
async fn wait_for_url(file: &std::path::Path) -> url::Url {
    for _ in 0..500 {
        if let Ok(text) = std::fs::read_to_string(file) {
            return url::Url::parse(&text).unwrap();
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("the opener was never called");
}

#[cfg(unix)]
#[test]
fn account_login_unknown_account_fails_before_the_opener() {
    let dir = tempfile::tempdir().unwrap();
    let config = write_config(&dir, WORK_FILE_BACKEND);
    let url_file = dir.path().join("url.txt");
    let out = rail_cmd(&dir)
        .args(["account", "login", "nobody"])
        .env("RAIL_TEST_OPENER", write_opener(&dir))
        .env("RAIL_TEST_URL_FILE", &url_file)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no account named or addressed"),
        "got {stderr:?}"
    );
    assert!(!url_file.exists(), "the opener must not run");
    assert_eq!(std::fs::read_to_string(config).unwrap(), WORK_FILE_BACKEND);
    assert!(!dir.path().join("data").exists());
}

#[cfg(unix)]
#[tokio::test]
async fn account_login_replaces_tokens_in_the_file_backend() {
    use rail_core::auth::{FileSecretStore, SecretStore, TokenSet};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let dir = tempfile::tempdir().unwrap();
    let config = write_config(&dir, WORK_FILE_BACKEND);
    let secrets = FileSecretStore::new(dir.path().join("data").join("secrets.json"));
    let old = TokenSet {
        access_token: "old-access".into(),
        refresh_token: "old-refresh".into(),
        expires_at: 1,
    };
    secrets.set_tokens("me@example.com", &old).unwrap();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains("code=the-code"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "access_token": "new-access", "refresh_token": "new-refresh",
            "expires_in": 3600, "token_type": "Bearer"
        })))
        .expect(1)
        .mount(&server)
        .await;

    let url_file = dir.path().join("url.txt");
    let child = rail_cmd(&dir)
        .args(["account", "login", "WORK"])
        .env("RAIL_TEST_OPENER", write_opener(&dir))
        .env("RAIL_TEST_URL_FILE", &url_file)
        .env("RAIL_TEST_TOKEN_URL", format!("{}/token", server.uri()))
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let consent = wait_for_url(&url_file).await;
    let state = query(&consent, "state");
    let redirect = query(&consent, "redirect_uri");
    reqwest::get(format!("{redirect}/?code=the-code&state={state}"))
        .await
        .unwrap();
    let out = tokio::task::spawn_blocking(move || child.wait_with_output())
        .await
        .unwrap()
        .unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stderr: {stderr}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("Logged in to me@example.com"),
        "got {stdout:?}"
    );
    let tokens = secrets.get_tokens("me@example.com").unwrap().unwrap();
    assert_eq!(tokens.access_token, "new-access");
    assert_eq!(tokens.refresh_token, "new-refresh");
    assert_eq!(std::fs::read_to_string(config).unwrap(), WORK_FILE_BACKEND);
}

/// Review focus 1: the wizard never starts in a pipe. `output()` pipes stdout.
fn assert_pipe_hint(dir: &tempfile::TempDir, args: &[&str]) {
    let out = rail_cmd(dir).args(args).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "{args:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("interactive terminal"),
        "{args:?}: {stderr:?}"
    );
    assert!(
        stderr.contains("rail account add <gmail|m365> --address"),
        "{args:?}: {stderr:?}"
    );
    assert!(
        out.stdout.is_empty(),
        "{args:?}: nothing on stdout, not even escape codes"
    );
    assert!(
        !dir.path().join("config").join("config.toml").exists(),
        "{args:?}"
    );
    assert!(!dir.path().join("data").exists(), "{args:?}");
}

#[test]
fn setup_refuses_to_start_when_stdout_is_piped() {
    let dir = tempfile::tempdir().unwrap();
    assert_pipe_hint(&dir, &["setup"]);
}

#[test]
fn bare_account_add_with_piped_stdout_prints_the_hint() {
    let dir = tempfile::tempdir().unwrap();
    assert_pipe_hint(&dir, &["account", "add"]);
}

#[test]
fn account_add_with_only_a_provider_prints_the_hint() {
    let dir = tempfile::tempdir().unwrap();
    assert_pipe_hint(&dir, &["account", "add", "m365"]);
}

#[test]
fn rail_without_config_and_piped_stdout_prints_the_hint() {
    let dir = tempfile::tempdir().unwrap();
    assert_pipe_hint(&dir, &[]);
}

/// Run `rail account add <args>` and return (exit code, stderr).
fn add_failure(dir: &tempfile::TempDir, args: &[&str]) -> (Option<i32>, String) {
    let out = rail_cmd(dir)
        .args(["account", "add"])
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn account_add_flags_still_need_a_provider_and_a_client_id() {
    let dir = tempfile::tempdir().unwrap();
    let (code, stderr) = add_failure(&dir, &["--address", "a@b.c", "--client-id", "x"]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("provider"), "{stderr:?}");
    let (code, stderr) = add_failure(&dir, &["gmail", "--address", "a@b.c"]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("--client-id"), "{stderr:?}");
    let (code, stderr) = add_failure(&dir, &["gmail", "--tenant", "t"]);
    assert_eq!(code, Some(1));
    assert!(stderr.contains("--address"), "{stderr:?}");
    assert!(!dir.path().join("data").exists());
}

#[test]
fn doctor_without_config_mentions_rail_setup() {
    let dir = tempfile::tempdir().unwrap();
    let out = rail_cmd(&dir).arg("doctor").output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("run `rail setup`"), "got {stdout:?}");
}

/// Review focus 5, end to end: the real binary in a pseudo-terminal reaches the login wait,
/// gets Ctrl-C (byte 0x03, a key in raw mode), restores the terminal and saves nothing.
#[cfg(target_os = "linux")]
mod pty {
    use std::io::{Read, Write};
    use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use super::{rail_env, write_config, write_opener};

    const FILE_BACKEND: &str = "[general]\nsecrets = \"file\"\n";
    const ACME: &str = "name = \"Acme Gmail\"\nkind = \"gmail\"\nclient_id = \"acme.apps.googleusercontent.com\"\nclient_secret = \"GOCSPX-acme\"\n";
    /// Bound on every wait; the child is killed when the test ends either way.
    const LIMIT: Duration = Duration::from_secs(30);

    /// Kills the child if the test fails half way, so no wizard is left running.
    struct Running(Child);

    impl Drop for Running {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn script_available() -> bool {
        Command::new("script")
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    }

    /// Everything the pseudo-terminal shows, collected on a thread.
    fn collect(mut out: impl Read + Send + 'static) -> Arc<Mutex<Vec<u8>>> {
        let buffer = Arc::new(Mutex::new(Vec::new()));
        let sink = buffer.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = out.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        buffer
    }

    fn text(buffer: &Mutex<Vec<u8>>) -> String {
        String::from_utf8_lossy(&buffer.lock().unwrap()).into_owned()
    }

    /// Poll `done` until it holds; on timeout fail with what the terminal showed.
    fn wait_until(what: &str, output: &Mutex<Vec<u8>>, mut done: impl FnMut() -> bool) {
        let deadline = Instant::now() + LIMIT;
        while !done() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; screen:\n{:?}",
                text(output)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Start `rail setup` under `script` with a preset available and an opener that never
    /// redirects back.
    fn start(dir: &tempfile::TempDir, url_file: &std::path::Path) -> (Running, ChildStdin) {
        let presets = dir.path().join("config").join("presets");
        std::fs::create_dir_all(&presets).unwrap();
        std::fs::write(presets.join("acme.toml"), ACME).unwrap();
        let command = format!(
            "stty cols 100 rows 30; exec '{}' setup",
            env!("CARGO_BIN_EXE_rail")
        );
        let mut script = Command::new("script");
        rail_env(&mut script, dir)
            .env("RAIL_TEST_OPENER", write_opener(dir))
            .env("RAIL_TEST_URL_FILE", url_file)
            .args(["-q", "-e", "-c", &command, "/dev/null"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = Running(script.spawn().unwrap());
        let keys = child.0.stdin.take().unwrap();
        (child, keys)
    }

    fn wait_for_exit(child: &mut Running, output: &Mutex<Vec<u8>>) -> ExitStatus {
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(status) = child.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "rail did not quit:\n{:?}",
                text(output)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    #[test]
    fn ctrl_c_during_login_wait_restores_the_terminal_and_saves_nothing() {
        if !script_available() {
            eprintln!("skipping: util-linux `script` is not installed");
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let config = write_config(&dir, FILE_BACKEND);
        let url_file = dir.path().join("url.txt");
        let (mut child, mut keys) = start(&dir, &url_file);
        let output = collect(child.0.stdout.take().unwrap());
        let seen = |needle: &'static str| {
            let output = output.clone();
            move || text(&output).contains(needle)
        };

        wait_until("the start step", &output, seen("Use a preset"));
        keys.write_all(b"\r").unwrap();
        wait_until("the preset list", &output, seen("Acme Gmail"));
        keys.write_all(b"\r").unwrap();
        wait_until("the details step", &output, seen("Email address"));
        keys.write_all(b"me@example.com\r").unwrap();
        wait_until("the browser to be asked", &output, || url_file.exists());
        keys.write_all(&[0x03]).unwrap();
        keys.flush().unwrap();

        let status = wait_for_exit(&mut child, &output);
        drop(keys);
        std::thread::sleep(Duration::from_millis(200));
        let out = text(&output);
        let entered = out
            .find("\x1b[?1049h")
            .expect("entered the alternate screen");
        let left = out.rfind("\x1b[?1049l").expect("left the alternate screen");
        assert!(left > entered, "{out:?}");
        assert!(out[left..].contains("setup cancelled"), "{out:?}");
        assert_eq!(status.code(), Some(1));
        assert_eq!(std::fs::read_to_string(config).unwrap(), FILE_BACKEND);
        assert!(
            !dir.path().join("data").join("secrets.json").exists(),
            "no tokens"
        );
        assert!(
            !dir.path().join("data").join("rail.db").exists(),
            "no store row"
        );
    }
}
