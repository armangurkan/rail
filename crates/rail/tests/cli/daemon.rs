//! The daemon through the real binary: starting on demand, `rail daemon run|status|stop`,
//! sharing one daemon, its directories and its signals.

use std::process::Command;

use super::{
    KillsChild, StopsDaemon, WORK_FILE_BACKEND, finish_within, opened_url_file, rail_cmd,
    stdout_of, write_config, write_opener,
};

#[test]
fn daemon_status_says_when_no_daemon_runs() {
    let dir = tempfile::tempdir().unwrap();
    let out = rail_cmd(&dir).args(["daemon", "status"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(
        stdout_of(&out).contains("rail daemon is not running"),
        "{out:?}"
    );
    assert!(
        !dir.path().join("run").join("rail.sock").exists(),
        "status never starts one"
    );
}

#[test]
fn a_command_starts_the_daemon_and_daemon_stop_ends_it() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, "[general]\nsecrets = \"file\"\n");
    let doctor = rail_cmd(&dir).arg("doctor").output().unwrap();
    assert!(doctor.status.success(), "{doctor:?}");
    assert!(
        stdout_of(&doctor).contains("ok   daemon   rail daemon running"),
        "{doctor:?}"
    );
    let status = rail_cmd(&dir).args(["daemon", "status"]).output().unwrap();
    assert!(status.status.success(), "{status:?}");
    assert!(
        stdout_of(&status).contains("rail daemon running: pid"),
        "{status:?}"
    );
    let stop = rail_cmd(&dir).args(["daemon", "stop"]).output().unwrap();
    assert!(stop.status.success(), "{stop:?}");
    assert!(stdout_of(&stop).contains("rail daemon stopped"), "{stop:?}");
    assert!(
        !dir.path().join("run").join("rail.sock").exists(),
        "the socket is gone once stop returns"
    );
    let after = rail_cmd(&dir).args(["daemon", "status"]).output().unwrap();
    assert_eq!(after.status.code(), Some(1), "{after:?}");
}

#[test]
fn daemon_stop_without_a_daemon_succeeds_quietly() {
    let dir = tempfile::tempdir().unwrap();
    let out = rail_cmd(&dir).args(["daemon", "stop"]).output().unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(
        stdout_of(&out).contains("rail daemon is not running"),
        "{out:?}"
    );
}

#[cfg(unix)]
#[test]
fn the_socket_directory_is_private_and_the_socket_0600() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, "[general]\nsecrets = \"file\"\n");
    rail_cmd(&dir).args(["account", "list"]).output().unwrap();
    let mode = |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(dir.path().join("run")), 0o700);
    assert_eq!(mode(dir.path().join("run").join("rail.sock")), 0o600);
    rail_cmd(&dir).args(["daemon", "stop"]).output().unwrap();
}

/// Review focus 2, with the real binary: commands started together share one daemon. Each
/// daemon logs "daemon listening" once when it binds; a second one would log it too, or fail
/// to bind and log why ("another rail daemon is running").
#[test]
fn concurrent_commands_share_one_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    let children: Vec<_> = (0..6)
        .map(|_| {
            rail_cmd(&dir)
                .args(["account", "list"])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "{out:?}");
        assert!(stdout_of(&out).contains("work"), "{out:?}");
    }
    rail_cmd(&dir).args(["daemon", "stop"]).output().unwrap();
    let log = std::fs::read_to_string(dir.path().join("state").join("rail.log")).unwrap();
    assert_eq!(log.matches("daemon listening").count(), 1, "{log}");
    assert!(!log.contains("the daemon could not start"), "{log}");
}

/// The P13 check: a daemon that cannot bind says why in `rail.log`, so the assertion above
/// that no such line appears can fail.
#[test]
fn a_second_daemon_logs_why_it_cannot_start() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, "[general]\nsecrets = \"file\"\n");
    rail_cmd(&dir).args(["account", "list"]).output().unwrap();
    let second = rail_cmd(&dir).args(["daemon", "run"]).output().unwrap();
    assert!(!second.status.success(), "{second:?}");
    rail_cmd(&dir).args(["daemon", "stop"]).output().unwrap();
    let log = std::fs::read_to_string(dir.path().join("state").join("rail.log")).unwrap();
    assert_eq!(
        log.matches("the daemon could not start").count(),
        1,
        "{log}"
    );
    assert!(log.contains("another rail daemon is running"), "{log}");
}

/// Relative `RAIL_*_DIR` settings name directories under the command's working
/// directory, and the daemon (which runs in `/`) works on those same directories.
#[test]
fn relative_directory_settings_work_through_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    let relative = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_rail"))
            .current_dir(dir.path())
            .env("RAIL_CONFIG_DIR", "config")
            .env("RAIL_DATA_DIR", "data")
            .env("RAIL_CACHE_DIR", "cache")
            .env("RAIL_STATE_DIR", "state")
            .env("RAIL_RUNTIME_DIR", "run")
            .env("RAIL_TEST_IDLE_SECS", "2")
            .env("RAIL_TEST_OPENER", write_opener(&dir))
            .env("RAIL_TEST_URL_FILE", opened_url_file(&dir))
            // As in `rail_env`: the account this daemon syncs can never reach Google.
            .env(
                "RAIL_TEST_GMAIL_URL",
                "http://127.0.0.1:9/gmail/v1/users/me/",
            )
            .env("RAIL_TEST_TOKEN_URL", "http://127.0.0.1:9/token")
            .env("RAIL_TEST_IDLE_WITH_ACCOUNTS", "1")
            .args(args)
            .output()
            .unwrap()
    };
    let list = relative(&["account", "list"]);
    assert!(list.status.success(), "{list:?}");
    assert!(stdout_of(&list).contains("me@example.com"), "{list:?}");
    // The absolute spelling reaches the same daemon: same directories, no refusal.
    // The account has no tokens, so its sync line fails the check; the daemon still answers.
    // The daemon's first sync attempt is asynchronous (slow on an instrumented build): wait,
    // bounded, for the sync line to leave "idle, never synced" before asserting on it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let (doctor, text) = loop {
        let doctor = rail_cmd(&dir).arg("doctor").output().unwrap();
        let text = stdout_of(&doctor);
        if !text.contains("idle, never synced") || std::time::Instant::now() >= deadline {
            break (doctor, text);
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    assert!(
        text.contains("ok   daemon   rail daemon running"),
        "{doctor:?}"
    );
    assert!(
        text.contains("FAIL sync     work: needs a login"),
        "{doctor:?}"
    );
    assert!(dir.path().join("data").join("rail.db").exists());
}

/// A command whose config, data or state directory differs from the running
/// daemon's (sharing its runtime directory) is refused and changes nothing on either side.
#[test]
fn a_daemon_with_other_directories_is_refused_and_nothing_changes() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let config = write_config(&dir, WORK_FILE_BACKEND);
    // Should the refusal ever regress, the login reaches only the stand-in browser and the
    // bounded wait below fails instead of hanging.
    let first = rail_cmd(&dir).args(["account", "list"]).output().unwrap();
    assert!(first.status.success(), "{first:?}");
    let other = dir.path().join("other-config");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("config.toml"), "[general]\nsecrets = \"file\"\n").unwrap();
    let add = rail_cmd(&dir)
        .env("RAIL_CONFIG_DIR", &other)
        .args([
            "account",
            "add",
            "gmail",
            "--address",
            "new@example.com",
            "--client-id",
            "x",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let add = finish_within(add, std::time::Duration::from_secs(20));
    assert!(!add.status.success(), "{add:?}");
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(
        stderr.contains("a Rail daemon is already running with different directories"),
        "{stderr}"
    );
    assert!(
        stderr.contains("hint: stop it with `rail daemon stop`"),
        "{stderr}"
    );
    assert!(stderr.contains("other-config"), "{stderr}");
    assert!(!stdout_of(&add).contains("http"), "{add:?}");
    assert!(!opened_url_file(&dir).exists(), "the browser was asked");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), WORK_FILE_BACKEND);
    assert_eq!(
        std::fs::read_to_string(other.join("config.toml")).unwrap(),
        "[general]\nsecrets = \"file\"\n"
    );
    let status = rail_cmd(&dir).args(["daemon", "status"]).output().unwrap();
    assert!(
        status.status.success(),
        "the first daemon still runs: {status:?}"
    );
}

/// SIGTERM or SIGINT to a foreground `rail daemon run` shuts it down like
/// `rail daemon stop`: exit 0, the socket removed and `daemon.lock` released.
#[cfg(unix)]
#[test]
fn signals_stop_a_foreground_daemon_cleanly() {
    for signal in ["-TERM", "-INT"] {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("run").join("rail.sock");
        let child = rail_cmd(&dir)
            .args(["daemon", "run"])
            .env("RAIL_TEST_IDLE_SECS", "600")
            .spawn()
            .unwrap();
        let mut running = KillsChild(Some(child));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !socket.exists() {
            assert!(
                std::time::Instant::now() < deadline,
                "the daemon never bound"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let pid = running.0.as_ref().unwrap().id().to_string();
        assert!(
            Command::new("kill")
                .args([signal, &pid])
                .status()
                .unwrap()
                .success()
        );
        let out = finish_within(
            running.0.take().unwrap(),
            std::time::Duration::from_secs(20),
        );
        assert_eq!(out.status.code(), Some(0), "{signal}");
        assert!(!socket.exists(), "{signal}: the socket is removed");
        rail_core::daemon::wait_for_daemon_exit(
            &dir.path().join("run").join("daemon.lock"),
            std::time::Duration::from_millis(500),
        )
        .unwrap_or_else(|e| panic!("{signal}: daemon.lock still held: {e:?}"));
        let log = std::fs::read_to_string(dir.path().join("state").join("rail.log")).unwrap();
        assert!(log.contains("asked to stop by a signal"), "{signal}: {log}");
    }
}

/// Directories are compared after resolving symlinks, so a config directory
/// reached through a symlink (a dotfiles setup, or macOS's `/var` -> `/private/var`) uses the
/// running daemon instead of being refused.
#[test]
fn a_config_dir_reached_through_a_symlink_shares_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    let link = dir.path().join("config-link");
    std::os::unix::fs::symlink(dir.path().join("config"), &link).unwrap();
    let first = rail_cmd(&dir).args(["account", "list"]).output().unwrap();
    assert!(first.status.success(), "{first:?}");
    let linked = rail_cmd(&dir)
        .env("RAIL_CONFIG_DIR", &link)
        .args(["account", "list"])
        .output()
        .unwrap();
    assert!(linked.status.success(), "{linked:?}");
    assert!(stdout_of(&linked).contains("me@example.com"), "{linked:?}");
    rail_cmd(&dir).args(["daemon", "stop"]).output().unwrap();
    let log = std::fs::read_to_string(dir.path().join("state").join("rail.log")).unwrap();
    assert_eq!(log.matches("daemon listening").count(), 1, "{log}");
}

/// Spec §3: a daemon started without a display still opens the browser where the user is.
#[test]
fn a_login_opens_the_browser_on_the_display_of_the_client_that_began_it() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    let seen = dir.path().join("display.txt");
    let opener = dir.path().join("display-opener.sh");
    std::fs::write(
        &opener,
        format!(
            "#!/bin/sh\nprintf '%s' \"$DISPLAY\" > '{0}.tmp'\nmv '{0}.tmp' '{0}'\n",
            seen.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&opener, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = rail_cmd(&dir)
        .env_remove("DISPLAY")
        .env("RAIL_TEST_OPENER", &opener)
        .args(["account", "list"])
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    let login = rail_cmd(&dir)
        .env("DISPLAY", ":42")
        .env("RAIL_TEST_OPENER", &opener)
        .args(["account", "login", "work"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let _login = KillsChild(Some(login));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !seen.exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the browser was never opened"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), ":42");
}
