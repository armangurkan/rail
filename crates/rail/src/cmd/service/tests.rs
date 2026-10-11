//! The install and uninstall steps against a fake service manager: every command is recorded,
//! none is run.

use std::cell::RefCell;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::ExitCode;
use std::time::Duration;

use super::flow::{
    check_started, install_with, put_in_place, remove, require_manager, write_unit_file,
};
use super::runner::{Outcome, Runner};
use super::*;

const PROBE: &str = "systemctl --user show --property=Version";
const ENABLE: &str = "systemctl --user enable --now rail.service";
const STATUS: &str = "systemctl --user --no-pager status rail.service";
const MAIN_PID: &str = "systemctl --user show --property=MainPID --value rail.service";

/// Records each command and answers with `answers` (success with no output otherwise);
/// `None` stands for a program that cannot be run.
#[derive(Default)]
struct Fake {
    ran: RefCell<Vec<String>>,
    answers: Vec<(&'static str, Option<Outcome>)>,
}

impl Fake {
    fn with(answers: Vec<(&'static str, Option<Outcome>)>) -> Self {
        Self {
            answers,
            ..Self::default()
        }
    }

    fn ran(self) -> Vec<String> {
        self.ran.into_inner()
    }
}

fn fails(stderr: &str) -> Option<Outcome> {
    Some(Outcome {
        ok: false,
        stdout: String::new(),
        stderr: stderr.into(),
    })
}

fn prints(stdout: &str) -> Option<Outcome> {
    Some(Outcome {
        ok: true,
        stdout: stdout.into(),
        stderr: String::new(),
    })
}

impl Runner for Fake {
    fn run(&self, command: &Cmd) -> std::io::Result<Outcome> {
        let shown = command.join(" ");
        self.ran.borrow_mut().push(shown.clone());
        match self.answers.iter().find(|(c, _)| *c == shown) {
            None => Ok(Outcome::success()),
            Some((_, Some(outcome))) => Ok(outcome.clone()),
            Some((_, None)) => Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
        }
    }
}

fn systemd_unit(root: &Path) -> Unit {
    let setup = Setup {
        exe: "/opt/rail/rail".into(),
        paths: Paths::all_in(&root.join("rail")),
        op_dir: None,
        home: root.join("home"),
        config_home: root.join("xdg"),
        uid: 1000,
    };
    systemd::unit(&setup).unwrap()
}

fn code(code: ExitCode) -> String {
    format!("{code:?}")
}

#[test]
fn putting_the_unit_in_place_twice_writes_one_file_and_enables_it_each_time() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    let fake = Fake::default();
    put_in_place(&unit, &fake).unwrap();
    assert_eq!(std::fs::read_to_string(&unit.path).unwrap(), unit.text);
    put_in_place(&unit, &fake).unwrap();
    assert_eq!(std::fs::read_to_string(&unit.path).unwrap(), unit.text);
    assert_eq!(
        fake.ran(),
        [
            "systemctl --user daemon-reload",
            ENABLE,
            "systemctl --user daemon-reload",
            ENABLE
        ]
    );
}

#[test]
fn the_unit_is_0644_and_an_existing_directory_keeps_its_mode() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    let units_dir = unit.path.parent().unwrap();
    std::fs::create_dir_all(units_dir).unwrap();
    std::fs::set_permissions(units_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    write_unit_file(&unit.path, &unit.text).unwrap();
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(units_dir), 0o755);
    assert_eq!(mode(&unit.path), 0o644);
    assert_eq!(
        std::fs::read_dir(units_dir).unwrap().count(),
        1,
        "no temp file left"
    );
}

#[test]
fn without_a_user_manager_nothing_is_installed_and_systemctl_says_why() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    let fake = Fake::with(vec![(
        PROBE,
        fails("Failed to connect to bus: No medium found"),
    )]);
    let error = require_manager(&unit, &fake).err().unwrap().to_string();
    assert!(error.contains("nothing was installed"), "{error}");
    assert!(error.contains("no user service manager answers"), "{error}");
    assert!(
        error
            .contains("from outside Rail (the provider or the system): \"Failed to connect to bus"),
        "{error}"
    );
    assert!(!unit.path.exists());
}

#[test]
fn a_missing_systemctl_is_not_called_a_missing_user_manager() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    let fake = Fake::with(vec![(PROBE, None)]);
    let error = require_manager(&unit, &fake).err().unwrap().to_string();
    assert!(error.contains("cannot be run here"), "{error}");
    assert!(error.contains("could not run `systemctl"), "{error}");
    assert!(
        !error.contains("no user service manager answers"),
        "{error}"
    );
}

#[test]
fn a_new_unit_that_cannot_be_enabled_is_removed_again() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    let fake = Fake::with(vec![(ENABLE, fails("Unit rail.service has a bad setting"))]);
    let error = format!("{:?}", put_in_place(&unit, &fake).err().unwrap());
    assert!(error.contains("enable --now"), "{error}");
    assert!(
        error.contains("Unit rail.service has a bad setting"),
        "{error}"
    );
    assert!(!unit.path.exists());
    assert_eq!(
        fake.ran(),
        [
            "systemctl --user daemon-reload",
            ENABLE,
            "systemctl --user disable --now rail.service",
            "systemctl --user daemon-reload",
        ]
    );
}

#[test]
fn a_failed_reinstall_puts_the_previous_unit_back() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    let old = unit.text.replace("/opt/rail/rail", "/old/rail");
    write_unit_file(&unit.path, &old).unwrap();
    let fake = Fake::with(vec![(ENABLE, fails("no"))]);
    assert!(put_in_place(&unit, &fake).is_err());
    assert_eq!(std::fs::read_to_string(&unit.path).unwrap(), old);
    assert_eq!(
        fake.ran(),
        [
            "systemctl --user daemon-reload",
            ENABLE,
            "systemctl --user daemon-reload"
        ],
        "the previous unit is not disabled"
    );
}

#[test]
fn removing_without_a_user_manager_still_removes_the_file_and_its_enable_link() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    put_in_place(&unit, &Fake::default()).unwrap();
    let marker = unit.enabled_marker.clone().unwrap();
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&unit.path, &marker).unwrap();
    let fake = Fake::with(vec![(PROBE, fails("Failed to connect to bus"))]);
    remove(&unit, &fake).unwrap();
    assert!(!unit.path.exists());
    assert!(std::fs::symlink_metadata(&marker).is_err());
    assert_eq!(fake.ran(), [PROBE]);
}

#[test]
fn a_failed_disable_is_reported_and_the_unit_still_removed() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    put_in_place(&unit, &Fake::default()).unwrap();
    let disable = "systemctl --user disable --now rail.service";
    let fake = Fake::with(vec![(disable, fails("Access denied"))]);
    remove(&unit, &fake).unwrap();
    assert!(!unit.path.exists());
    assert_eq!(
        fake.ran(),
        [PROBE, disable, "systemctl --user daemon-reload"]
    );
}

#[tokio::test]
async fn a_unit_whose_daemon_never_answers_is_reported_with_its_state() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    let paths = Paths::all_in(dir.path());
    let fake = Fake::with(vec![(STATUS, prints("Active: failed (Result: exit-code)"))]);
    let exit = install_with(&paths, &unit, &fake, Some(Duration::from_millis(300)))
        .await
        .unwrap();
    assert_eq!(code(exit), code(ExitCode::from(1)));
    assert!(unit.path.exists(), "the unit stays installed");
    assert_eq!(
        fake.ran(),
        [PROBE, "systemctl --user daemon-reload", ENABLE, STATUS]
    );
}

#[tokio::test]
async fn the_unit_is_running_when_its_own_daemon_answers() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    let paths = Paths::all_in(dir.path());
    let _daemon = crate::tui::test_util::daemon_ctx(&paths, None).await;
    let own = std::process::id().to_string();
    let fake = Fake::with(vec![(MAIN_PID, prints(&own))]);
    let exit = check_started(&paths, &unit, &fake, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(code(exit), code(ExitCode::SUCCESS));
    assert_eq!(fake.ran(), [MAIN_PID]);
}

#[tokio::test]
async fn a_daemon_another_command_started_in_between_is_not_taken_for_the_units() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    let paths = Paths::all_in(dir.path());
    let _daemon = crate::tui::test_util::daemon_ctx(&paths, None).await;
    let fake = Fake::with(vec![(MAIN_PID, prints("0"))]);
    let exit = check_started(&paths, &unit, &fake, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(code(exit), code(ExitCode::from(1)));
}

#[test]
fn autostart_tells_missing_enabled_and_stale_units_apart() {
    let dir = tempfile::tempdir().unwrap();
    let unit = systemd_unit(dir.path());
    assert!(matches!(autostart(&unit), Autostart::NotInstalled));
    put_in_place(&unit, &Fake::default()).unwrap();
    assert!(matches!(
        autostart(&unit),
        Autostart::Installed {
            enabled: false,
            current: true,
            ..
        }
    ));
    let marker = unit.enabled_marker.clone().unwrap();
    std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&unit.path, &marker).unwrap();
    assert!(matches!(
        autostart(&unit),
        Autostart::Installed {
            enabled: true,
            current: true,
            ..
        }
    ));
    let moved = unit.text.replace("/opt/rail/rail", "/old/place/rail");
    std::fs::write(&unit.path, moved).unwrap();
    assert!(matches!(
        autostart(&unit),
        Autostart::Installed { current: false, .. }
    ));
}

#[test]
fn the_doctor_line_for_a_config_that_does_not_load_is_one_line() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(dir.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), "[general\nbroken = ").unwrap();
    let line = autostart_line(&paths);
    assert!(line.starts_with("skip login    cannot tell: "), "{line}");
    assert!(!line.contains('\n'), "{line}");
}

#[test]
fn a_path_with_a_control_character_is_refused_and_shown_escaped() {
    let error = text_of(Path::new("/a\nb")).err().unwrap().to_string();
    assert!(error.contains("/a\\nb"), "{error}");
    assert!(!error.contains('\n'), "{error}");
}

/// The launchd domain (`gui/<uid>`) is the running user's: the uid comes from the process, not
/// from the owner of `HOME`. Here `HOME` is `/`, owned by root, as a `sudo -E` or a shared home
/// would give.
#[test]
fn the_uid_is_the_running_users_not_the_owner_of_home() {
    let dir = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(dir.path());
    let setup = setup_in(&paths, "/".into(), "/.config".into()).unwrap();
    let mine = std::fs::metadata(dir.path()).unwrap().uid();
    assert_eq!(
        setup.uid, mine,
        "the test's own files are the running user's"
    );
    assert_eq!(setup.uid, rustix::process::getuid().as_raw());
}
