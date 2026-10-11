//! `rail daemon install|uninstall` in a temporary home; systemctl and launchctl are never run:
//! the commands that would run are logged to a file instead.

use super::{StopsDaemon, WORK_FILE_BACKEND, rail_cmd, stdout_of, write_config};

/// What the log holds before the commands of each step on Linux: the check that a user
/// service manager answers.
const PROBE: &str = "systemctl --user show --property=Version\n";

struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn cmd(&self, args: &[&str]) -> std::process::Output {
        rail_cmd(&self.dir)
            .env("HOME", self.dir.path().join("home"))
            .env("XDG_CONFIG_HOME", self.dir.path().join("xdg"))
            .env("RAIL_TEST_SERVICE_LOG", self.log())
            .args(args)
            .output()
            .unwrap()
    }

    fn log(&self) -> std::path::PathBuf {
        self.dir.path().join("service.log")
    }

    fn logged(&self) -> String {
        std::fs::read_to_string(self.log()).unwrap_or_default()
    }

    fn unit(&self) -> std::path::PathBuf {
        if cfg!(target_os = "macos") {
            self.dir
                .path()
                .join("home/Library/LaunchAgents/io.github.vitosans.rail.plist")
        } else {
            self.dir.path().join("xdg/systemd/user/rail.service")
        }
    }
}

#[test]
fn install_writes_the_unit_and_enables_it_without_touching_the_real_service_manager() {
    let home = Home::new();
    std::fs::create_dir_all(home.dir.path().join("home")).unwrap();
    let out = home.cmd(&["daemon", "install"]);
    assert!(out.status.success(), "{out:?}");
    let text = std::fs::read_to_string(home.unit()).unwrap();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_rail")).unwrap();
    assert!(text.contains(&exe.display().to_string()), "{text}");
    assert!(
        text.contains(&home.dir.path().join("config").display().to_string()),
        "{text}"
    );
    let stdout = stdout_of(&out);
    assert!(stdout.contains("Wrote "), "{stdout}");
    assert!(stdout.contains("starts at login"), "{stdout}");
    if cfg!(target_os = "linux") {
        assert_eq!(
            home.logged(),
            format!(
                "{PROBE}systemctl --user daemon-reload\nsystemctl --user enable --now rail.service\n"
            )
        );
    }
    let again = home.cmd(&["daemon", "install"]);
    assert!(again.status.success(), "{again:?}");
    assert!(
        stdout_of(&again).contains("already up to date"),
        "{again:?}"
    );
    assert_eq!(std::fs::read_to_string(home.unit()).unwrap(), text);
}

#[test]
fn install_stops_a_daemon_that_is_already_running() {
    let home = Home::new();
    std::fs::create_dir_all(home.dir.path().join("home")).unwrap();
    let _daemon = StopsDaemon(&home.dir);
    write_config(&home.dir, WORK_FILE_BACKEND);
    let started = rail_cmd(&home.dir)
        .env_remove("RAIL_TEST_IDLE_WITH_ACCOUNTS")
        .args(["account", "list"])
        .output()
        .unwrap();
    assert!(started.status.success(), "{started:?}");
    let out = home.cmd(&["daemon", "install"]);
    assert!(out.status.success(), "{out:?}");
    assert!(
        stdout_of(&out).contains("Stopped the running rail daemon"),
        "{out:?}"
    );
    let status = home.cmd(&["daemon", "status"]);
    assert_eq!(
        status.status.code(),
        Some(1),
        "only the unit's daemon may run now: {status:?}"
    );
}

#[test]
fn uninstall_removes_the_unit_and_is_idempotent() {
    let home = Home::new();
    std::fs::create_dir_all(home.dir.path().join("home")).unwrap();
    let nothing = home.cmd(&["daemon", "uninstall"]);
    assert!(nothing.status.success(), "{nothing:?}");
    assert!(stdout_of(&nothing).contains("not installed"), "{nothing:?}");
    assert!(
        stdout_of(&nothing).contains("nothing to remove"),
        "{nothing:?}"
    );
    assert!(home.cmd(&["daemon", "install"]).status.success());
    std::fs::remove_file(home.log()).unwrap();
    let out = home.cmd(&["daemon", "uninstall"]);
    assert!(out.status.success(), "{out:?}");
    assert!(!home.unit().exists());
    assert!(stdout_of(&out).contains("Removed "), "{out:?}");
    if cfg!(target_os = "linux") {
        assert_eq!(
            home.logged(),
            format!(
                "{PROBE}systemctl --user disable --now rail.service\nsystemctl --user daemon-reload\n"
            )
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn doctor_says_whether_the_login_unit_is_installed_and_enabled() {
    let home = Home::new();
    std::fs::create_dir_all(home.dir.path().join("home")).unwrap();
    let _daemon = StopsDaemon(&home.dir);
    write_config(&home.dir, "[general]\nsecrets = \"file\"\n");
    let before = stdout_of(&home.cmd(&["doctor"]));
    assert!(
        before.contains("skip login    not installed; `rail daemon install`"),
        "{before}"
    );
    assert!(home.cmd(&["daemon", "install"]).status.success());
    let installed = stdout_of(&home.cmd(&["doctor"]));
    assert!(
        installed.contains("warn login    rail.service is installed but not enabled"),
        "{installed}"
    );
    let wants = home
        .dir
        .path()
        .join("xdg/systemd/user/default.target.wants");
    std::fs::create_dir_all(&wants).unwrap();
    std::os::unix::fs::symlink(home.unit(), wants.join("rail.service")).unwrap();
    let enabled = stdout_of(&home.cmd(&["doctor"]));
    assert!(
        enabled.contains("ok   login    rail.service is installed and enabled"),
        "{enabled}"
    );
    let text = std::fs::read_to_string(home.unit()).unwrap();
    let exe = std::fs::canonicalize(env!("CARGO_BIN_EXE_rail")).unwrap();
    let moved = text.replace(&exe.display().to_string(), "/old/place/rail");
    std::fs::write(home.unit(), moved).unwrap();
    let stale = stdout_of(&home.cmd(&["doctor"]));
    assert!(
        stale.contains("warn login    rail.service does not run this rail binary"),
        "{stale}"
    );
}
