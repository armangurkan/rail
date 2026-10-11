//! The real binary in a pseudo-terminal, through util-linux `script`. Review focus 5, end to
//! end: the wizard reaches the login wait, gets Ctrl-C (byte 0x03, a key in raw mode), restores
//! the terminal and saves nothing. And `rail account remove` asks, and removes only on a yes.

use std::io::{Read, Write};
use std::process::{Child, ChildStdin, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{
    StopsDaemon, WORK_FILE_BACKEND, rail_env, store_work_tokens, write_config, write_opener,
};

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
/// Start `rail <args>` under `script`, so stdin and stdout are a terminal. `args` is shell
/// text, quoted by the caller.
fn start_in_terminal(dir: &tempfile::TempDir, args: &str) -> (Running, ChildStdin) {
    let command = format!("exec '{}' {args}", env!("CARGO_BIN_EXE_rail"));
    let mut script = Command::new("script");
    rail_env(&mut script, dir)
        .args(["-q", "-e", "-c", &command, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Running(script.spawn().unwrap());
    let keys = child.0.stdin.take().unwrap();
    (child, keys)
}

/// Answer the removal prompt with `answer`; returns the exit status and the screen.
fn answer_removal(dir: &tempfile::TempDir, answer: &[u8]) -> (ExitStatus, String) {
    answer_removal_after(dir, answer, || {})
}

/// `answer_removal`, running `meanwhile` while the question is on the screen.
fn answer_removal_after(
    dir: &tempfile::TempDir,
    answer: &[u8],
    meanwhile: impl FnOnce(),
) -> (ExitStatus, String) {
    answer_in_terminal(
        dir,
        "account remove 'ME@example.com'",
        "Remove? [y/N]",
        answer,
        meanwhile,
    )
}

/// Run `rail <args>` in a terminal, wait for `prompt`, run `meanwhile`, type `answer`; returns
/// the exit status and the screen.
fn answer_in_terminal(
    dir: &tempfile::TempDir,
    args: &str,
    prompt: &'static str,
    answer: &[u8],
    meanwhile: impl FnOnce(),
) -> (ExitStatus, String) {
    let (mut child, mut keys) = start_in_terminal(dir, args);
    let output = collect(child.0.stdout.take().unwrap());
    let shown = output.clone();
    wait_until("the prompt", &output, move || text(&shown).contains(prompt));
    meanwhile();
    keys.write_all(answer).unwrap();
    keys.flush().unwrap();
    let status = wait_for_exit(&mut child, &output);
    drop(keys);
    std::thread::sleep(Duration::from_millis(200));
    (status, text(&output))
}

#[test]
fn account_remove_on_a_terminal_removes_after_yes() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let config = write_config(&dir, WORK_FILE_BACKEND);
    let secrets = store_work_tokens(&dir);
    let (status, screen) = answer_removal(&dir, b"Yes\r");
    assert_eq!(status.code(), Some(0), "{screen:?}");
    assert!(
        screen.contains("Remove account \"work\" (\"me@example.com\", gmail)?"),
        "{screen:?}"
    );
    assert!(screen.contains("Mail already synced stays"), "{screen:?}");
    assert!(
        screen.contains("Removed \"work\" <me@example.com>"),
        "{screen:?}"
    );
    let left = std::fs::read_to_string(&config).unwrap();
    assert!(!left.contains("me@example.com"), "{left}");
    assert!(left.contains("secrets = \"file\""), "{left}");
    let tokens = std::fs::read_to_string(&secrets).unwrap();
    assert!(!tokens.contains("me@example.com"), "{tokens}");
}

/// The account changes while the question is on the screen; the yes was for the
/// old account, so nothing is removed and the user is told to run the command again.
#[test]
fn account_remove_after_the_account_changed_removes_nothing() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let config = write_config(&dir, WORK_FILE_BACKEND);
    let changed = WORK_FILE_BACKEND.replace("kind = \"gmail\"", "kind = \"m365\"");
    let edit = changed.clone();
    let path = config.clone();
    let (status, screen) =
        answer_removal_after(&dir, b"y\r", move || std::fs::write(&path, edit).unwrap());
    assert_eq!(status.code(), Some(1), "{screen:?}");
    assert!(
        screen.contains("the account changed while you answered; nothing was removed"),
        "{screen:?}"
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), changed);
}

#[test]
fn account_remove_on_a_terminal_keeps_everything_after_no() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let config = write_config(&dir, WORK_FILE_BACKEND);
    let secrets = store_work_tokens(&dir);
    let before = std::fs::read_to_string(&secrets).unwrap();
    let (status, screen) = answer_removal(&dir, b"\r");
    assert_eq!(status.code(), Some(1), "{screen:?}");
    assert!(screen.contains("nothing was removed"), "{screen:?}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), WORK_FILE_BACKEND);
    assert_eq!(std::fs::read_to_string(&secrets).unwrap(), before);
}

/// No account has tokens, so the keyring is never asked: only the config changes.
const KEYRING_BACKEND: &str = "[general]\nsecrets = \"keyring\"\n";

/// Final review F4: `rail secrets set` shows the daemon's text, asks, and changes the setting
/// on a yes.
#[test]
fn secrets_set_on_a_terminal_changes_the_store_after_yes() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let config = write_config(&dir, KEYRING_BACKEND);
    let (status, screen) =
        answer_in_terminal(&dir, "secrets set file", "Change? [y/N]", b"y\r", || {});
    assert_eq!(status.code(), Some(0), "{screen:?}");
    assert!(
        screen.contains("from keyring to file") && screen.contains("plain 0600 file"),
        "{screen:?}"
    );
    assert!(
        screen.contains("Tokens now go to the file backend"),
        "{screen:?}"
    );
    assert!(
        std::fs::read_to_string(&config)
            .unwrap()
            .contains("secrets = \"file\"")
    );
}

/// An empty answer is a no: nothing changes.
#[test]
fn secrets_set_on_a_terminal_changes_nothing_after_an_empty_answer() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let config = write_config(&dir, KEYRING_BACKEND);
    let (status, screen) =
        answer_in_terminal(&dir, "secrets set file", "Change? [y/N]", b"\r", || {});
    assert_eq!(status.code(), Some(1), "{screen:?}");
    assert!(screen.contains("nothing changed"), "{screen:?}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), KEYRING_BACKEND);
}

/// The settings change while the question is on the screen: the yes was for the old plan.
#[test]
fn secrets_set_after_the_settings_changed_changes_nothing() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let config = write_config(&dir, KEYRING_BACKEND);
    // A vault setting enters the plan; no account means the keyring is still never asked.
    let edited = format!("{KEYRING_BACKEND}\n[onepassword]\nvault = \"Other\"\n");
    let (path, edit) = (config.clone(), edited.clone());
    let (status, screen) = answer_in_terminal(
        &dir,
        "secrets set file",
        "Change? [y/N]",
        b"y\r",
        move || std::fs::write(&path, edit).unwrap(),
    );
    assert_eq!(status.code(), Some(1), "{screen:?}");
    assert!(
        screen.contains("the settings changed while you answered; nothing changed — run it again"),
        "{screen:?}"
    );
    assert_eq!(std::fs::read_to_string(&config).unwrap(), edited);
}

/// A setting already in place needs no question.
#[test]
fn secrets_set_to_the_current_store_says_nothing_needed_changing() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let config = write_config(&dir, FILE_BACKEND);
    let (mut child, keys) = start_in_terminal(&dir, "secrets set file");
    let output = collect(child.0.stdout.take().unwrap());
    let status = wait_for_exit(&mut child, &output);
    drop(keys);
    std::thread::sleep(Duration::from_millis(200));
    let screen = text(&output);
    assert_eq!(status.code(), Some(0), "{screen:?}");
    assert!(screen.contains("already in place"), "{screen:?}");
    assert!(!screen.contains("Change? [y/N]"), "{screen:?}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), FILE_BACKEND);
}
