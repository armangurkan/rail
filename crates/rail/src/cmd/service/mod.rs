//! `rail daemon install` / `uninstall`: start Rail's daemon at login with a systemd user unit
//! (Linux) or a launchd agent (macOS). The unit runs the canonical path of the binary that
//! installed it with the installing user's resolved `RAIL_*_DIR` directories and a plain
//! `PATH`. Both commands are idempotent and print what they did. Every service-manager command
//! goes through a `Runner`, which the binary tests replace with a log.

pub mod launchd;
pub mod systemd;

use std::io::Write as _;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{ExitCode, Stdio};

use color_eyre::eyre::{WrapErr, bail, eyre};
use rail_core::paths::Paths;

use crate::cmd::account::report;
use crate::cmd::text::escape_field;
use crate::{daemon_link, test_hooks};

/// The directories a unit's `PATH` always has.
pub const SYSTEM_PATH: [&str; 5] = ["/usr/local/bin", "/usr/bin", "/bin", "/usr/sbin", "/sbin"];

/// A command line for the service manager.
pub type Cmd = Vec<String>;

/// What a unit is built from: only values Rail resolved itself, never anything a client sent.
pub struct Setup {
    pub exe: PathBuf,
    pub paths: Paths,
    /// The directory of `op_path`, when config sets an absolute one.
    pub op_dir: Option<PathBuf>,
    pub home: PathBuf,
    pub config_home: PathBuf,
    pub uid: u32,
}

pub struct Unit {
    pub name: String,
    pub path: PathBuf,
    pub text: String,
    /// A file whose presence means the unit is enabled; `None`: installed means enabled.
    pub enabled_marker: Option<PathBuf>,
    /// A command that succeeds only when the service manager answers; `None`: it always does.
    pub probe: Option<Cmd>,
    /// Run before writing; failures are fine (nothing loaded yet).
    pub before_install: Vec<Cmd>,
    pub after_install: Vec<Cmd>,
    /// Run before removing; failures are fine (not loaded).
    pub before_remove: Vec<Cmd>,
    pub after_remove: Vec<Cmd>,
}

pub enum Autostart {
    NotInstalled,
    /// `current`: the file is exactly what this binary would write now (same binary, same
    /// directories, same `op_path`).
    Installed {
        path: PathBuf,
        enabled: bool,
        current: bool,
    },
}

/// Runs service-manager commands: `Ok(true)` when one succeeded.
pub trait Runner {
    fn run(&self, command: &Cmd) -> std::io::Result<bool>;
}

/// The real `systemctl` / `launchctl`, with stdin and stdout closed (a probe prints nothing).
struct System;

impl Runner for System {
    fn run(&self, command: &Cmd) -> std::io::Result<bool> {
        let Some((program, args)) = command.split_first() else {
            return Ok(true);
        };
        let status = std::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()?;
        Ok(status.success())
    }
}

/// Test hook: append each command to a file and report success; nothing runs.
struct Log(PathBuf);

impl Runner for Log {
    fn run(&self, command: &Cmd) -> std::io::Result<bool> {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.0)?;
        writeln!(file, "{}", command.join(" "))?;
        Ok(true)
    }
}

fn runner() -> Box<dyn Runner> {
    match test_hooks::service_log() {
        Some(log) => Box::new(Log(log)),
        None => Box::new(System),
    }
}

pub(crate) fn cmd(args: &[&str]) -> Cmd {
    args.iter().map(|a| (*a).to_string()).collect()
}

/// `path` as text for a unit, refused when it is not UTF-8 or holds a control character
/// (a newline would end the setting and start another).
pub(crate) fn text_of(path: &Path) -> color_eyre::Result<String> {
    match path.to_str() {
        Some(text) if !text.chars().any(char::is_control) => Ok(text.to_string()),
        _ => bail!(
            "{} cannot go into a login unit (not UTF-8, or a control character)",
            escape_field(&path.to_string_lossy(), usize::MAX)
        ),
    }
}

/// The variables the unit sets: Rail's five directories and `PATH`.
pub fn environment(setup: &Setup) -> color_eyre::Result<Vec<(String, String)>> {
    let p = &setup.paths;
    let mut vars = Vec::new();
    for (key, dir) in [
        ("RAIL_CONFIG_DIR", &p.config_dir),
        ("RAIL_DATA_DIR", &p.data_dir),
        ("RAIL_CACHE_DIR", &p.cache_dir),
        ("RAIL_STATE_DIR", &p.state_dir),
        ("RAIL_RUNTIME_DIR", &p.runtime_dir),
    ] {
        vars.push((key.to_string(), text_of(dir)?));
    }
    let mut path: Vec<String> = SYSTEM_PATH.iter().map(|d| (*d).to_string()).collect();
    if let Some(op_dir) = &setup.op_dir {
        let dir = text_of(op_dir)?;
        if dir.contains(':') {
            bail!("the directory of op_path ({dir}) contains ':', so it cannot go on PATH");
        }
        path.push(dir);
    }
    vars.push(("PATH".to_string(), path.join(":")));
    Ok(vars)
}

/// `HOME` (required) and `XDG_CONFIG_HOME` (when absolute, else `~/.config`).
fn homes() -> color_eyre::Result<(PathBuf, PathBuf)> {
    let home = std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| eyre!("HOME is not set, so Rail cannot find where login units go"))?;
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    Ok((home, config_home))
}

pub fn setup(paths: &Paths) -> color_eyre::Result<Setup> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .wrap_err("finding the rail binary")?;
    let (home, config_home) = homes()?;
    let uid = std::fs::metadata(&home).wrap_err("reading HOME")?.uid();
    let (_, config) = rail_core::config::load_text_or_empty(&paths.config_file())
        .map_err(|e| report(e.into()))?;
    let op_dir = config
        .onepassword
        .and_then(|o| o.op_path)
        .filter(|p| p.is_absolute())
        .and_then(|p| p.parent().map(Path::to_path_buf));
    Ok(Setup {
        exe,
        paths: paths.clone(),
        op_dir,
        home,
        config_home,
        uid,
    })
}

pub fn unit(setup: &Setup) -> color_eyre::Result<Unit> {
    if cfg!(target_os = "macos") {
        launchd::unit(setup)
    } else {
        systemd::unit(setup)
    }
}

pub fn autostart(unit: &Unit) -> Autostart {
    let Ok(text) = std::fs::read_to_string(&unit.path) else {
        if unit.path.exists() {
            return Autostart::Installed {
                path: unit.path.clone(),
                enabled: false,
                current: false,
            };
        }
        return Autostart::NotInstalled;
    };
    let enabled = unit
        .enabled_marker
        .as_ref()
        .is_none_or(|marker| marker.exists());
    Autostart::Installed {
        path: unit.path.clone(),
        enabled,
        current: text == unit.text,
    }
}

/// The `rail doctor` row. Never a failure: starting at login is optional.
pub fn autostart_line(paths: &Paths) -> String {
    let (setup, unit) = match setup(paths).and_then(|s| unit(&s).map(|u| (s, u))) {
        Ok(both) => both,
        Err(error) => {
            // The first line only: a config error's details are on doctor's config line.
            let text = error.to_string();
            let first = text.lines().next().unwrap_or_default();
            return format!("skip login    cannot tell: {first}");
        }
    };
    let name = &unit.name;
    match autostart(&unit) {
        Autostart::NotInstalled => {
            "skip login    not installed; `rail daemon install` starts Rail's daemon at login"
                .to_string()
        }
        Autostart::Installed {
            current: false,
            path,
            ..
        } => format!(
            "warn login    {name} does not run this rail binary ({}) with these directories (moved, or written by another install?); run `rail daemon install` to update {}",
            escape_field(&setup.exe.to_string_lossy(), usize::MAX),
            escape_field(&path.to_string_lossy(), usize::MAX)
        ),
        Autostart::Installed { enabled: true, .. } => {
            format!("ok   login    {name} is installed and enabled (starts at login)")
        }
        Autostart::Installed { enabled: false, .. } => {
            format!("warn login    {name} is installed but not enabled; run `rail daemon install`")
        }
    }
}

pub async fn install(paths: &Paths) -> color_eyre::Result<ExitCode> {
    let unit = unit(&setup(paths)?)?;
    let runner = runner();
    require_manager(&unit, &*runner)?;
    stop_running_daemon(paths).await?;
    put_in_place(&unit, &*runner)?;
    println!("Rail's daemon now starts at login ({}).", unit.name);
    Ok(ExitCode::SUCCESS)
}

pub async fn uninstall(paths: &Paths) -> color_eyre::Result<ExitCode> {
    let unit = unit(&setup(paths)?)?;
    if !unit.path.exists() {
        println!("Rail's login unit is not installed; nothing to remove.");
        return Ok(ExitCode::SUCCESS);
    }
    remove(&unit, &*runner())?;
    Ok(ExitCode::SUCCESS)
}

/// Whether the unit's service manager answers (always, without a probe).
fn manager_answers(unit: &Unit, runner: &dyn Runner) -> bool {
    unit.probe
        .as_ref()
        .is_none_or(|probe| runner.run(probe).unwrap_or(false))
}

/// Refuse before anything changes when there is no service manager to enable the unit with
/// (some containers have no `systemd --user`).
fn require_manager(unit: &Unit, runner: &dyn Runner) -> color_eyre::Result<()> {
    if manager_answers(unit, runner) {
        return Ok(());
    }
    let probe = unit.probe.as_ref().map(|p| p.join(" ")).unwrap_or_default();
    bail!(
        "`{probe}` failed: there is no user service manager to start Rail's daemon at login, so nothing was installed\nhint: without one, Rail still starts its daemon on demand"
    )
}

/// Write the unit (unless it is already exactly this) and enable it. If enabling fails, the
/// unit is disabled and removed again, so nothing is left half-installed.
fn put_in_place(unit: &Unit, runner: &dyn Runner) -> color_eyre::Result<()> {
    run(&unit.before_install, runner, true)?;
    if std::fs::read_to_string(&unit.path).ok().as_deref() == Some(unit.text.as_str()) {
        println!("{} is already up to date", unit.path.display());
    } else {
        if let Some(dir) = unit.path.parent() {
            std::fs::create_dir_all(dir).wrap_err_with(|| format!("creating {}", dir.display()))?;
        }
        rail_core::paths::write_private_atomic(&unit.path, unit.text.as_bytes())
            .wrap_err_with(|| format!("writing {}", unit.path.display()))?;
        println!("Wrote {}", unit.path.display());
    }
    if let Err(error) = run(&unit.after_install, runner, false) {
        run(&unit.before_remove, runner, true)?;
        std::fs::remove_file(&unit.path)
            .wrap_err_with(|| format!("removing {}", unit.path.display()))?;
        run(&unit.after_remove, runner, true)?;
        println!(
            "Removed {} again, so nothing is left half-installed",
            unit.path.display()
        );
        return Err(error);
    }
    Ok(())
}

/// Stop and disable the unit, then remove its file (and an enable link left behind when no
/// service manager answers).
fn remove(unit: &Unit, runner: &dyn Runner) -> color_eyre::Result<()> {
    let manager = manager_answers(unit, runner);
    if manager {
        run(&unit.before_remove, runner, true)?;
    } else {
        println!("No user service manager answers, so there is nothing to stop or disable");
    }
    std::fs::remove_file(&unit.path)
        .wrap_err_with(|| format!("removing {}", unit.path.display()))?;
    println!("Removed {}", unit.path.display());
    if let Some(marker) = &unit.enabled_marker
        && std::fs::symlink_metadata(marker).is_ok()
    {
        std::fs::remove_file(marker).wrap_err_with(|| format!("removing {}", marker.display()))?;
        println!("Removed {}", marker.display());
    }
    if manager {
        run(&unit.after_remove, runner, false)?;
    }
    Ok(())
}

/// Only one daemon may run: stop one started on demand before the unit starts its own.
async fn stop_running_daemon(paths: &Paths) -> color_eyre::Result<()> {
    let connector = daemon_link::connector(paths);
    if let Some(client) = connector.connect_existing().await.map_err(report)? {
        let pid = client.daemon().pid;
        connector.stop(client).await.map_err(report)?;
        println!("Stopped the running rail daemon (pid {pid}) so that the login unit runs it");
    }
    Ok(())
}

/// Run each command, printing it; `tolerate` ignores failures (for steps whose failure only
/// means there was nothing to undo).
fn run(commands: &[Cmd], runner: &dyn Runner, tolerate: bool) -> color_eyre::Result<()> {
    for command in commands {
        let shown = command.join(" ");
        match runner.run(command) {
            Ok(true) => println!("Ran: {shown}"),
            _ if tolerate => println!("Ran: {shown} (it failed: nothing to undo)"),
            Ok(false) => bail!("`{shown}` failed"),
            Err(error) => bail!("could not run `{shown}`: {error}"),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    /// Records each command; fails those listed in `failing`.
    #[derive(Default)]
    struct Fake {
        ran: RefCell<Vec<String>>,
        failing: Vec<&'static str>,
    }

    impl Runner for Fake {
        fn run(&self, command: &Cmd) -> std::io::Result<bool> {
            let shown = command.join(" ");
            let ok = !self.failing.contains(&shown.as_str());
            self.ran.borrow_mut().push(shown);
            Ok(ok)
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
            fake.ran.into_inner(),
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now rail.service",
                "systemctl --user daemon-reload",
                "systemctl --user enable --now rail.service",
            ]
        );
    }

    #[test]
    fn without_a_user_manager_nothing_is_installed() {
        let dir = tempfile::tempdir().unwrap();
        let unit = systemd_unit(dir.path());
        let fake = Fake {
            failing: vec!["systemctl --user show --property=Version"],
            ..Fake::default()
        };
        let error = require_manager(&unit, &fake).err().unwrap().to_string();
        assert!(error.contains("systemctl --user"), "{error}");
        assert!(error.contains("nothing was installed"), "{error}");
        assert!(!unit.path.exists());
    }

    #[test]
    fn a_unit_that_cannot_be_enabled_is_removed_again() {
        let dir = tempfile::tempdir().unwrap();
        let unit = systemd_unit(dir.path());
        let fake = Fake {
            failing: vec!["systemctl --user enable --now rail.service"],
            ..Fake::default()
        };
        let error = put_in_place(&unit, &fake).err().unwrap();
        assert!(format!("{error:?}").contains("enable --now"), "{error:?}");
        assert!(!unit.path.exists());
        assert_eq!(
            fake.ran.into_inner(),
            [
                "systemctl --user daemon-reload",
                "systemctl --user enable --now rail.service",
                "systemctl --user disable --now rail.service",
                "systemctl --user daemon-reload",
            ]
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
        let fake = Fake {
            failing: vec!["systemctl --user show --property=Version"],
            ..Fake::default()
        };
        remove(&unit, &fake).unwrap();
        assert!(!unit.path.exists());
        assert!(std::fs::symlink_metadata(&marker).is_err());
        assert_eq!(
            fake.ran.into_inner(),
            ["systemctl --user show --property=Version"]
        );
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
}
