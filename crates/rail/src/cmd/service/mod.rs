//! `rail daemon install` / `uninstall`: start Rail's daemon at login with a systemd user unit
//! (Linux) or a launchd agent (macOS). The unit runs the canonical path of the binary that
//! installed it with the installing user's resolved `RAIL_*_DIR` directories and a plain
//! `PATH`. Both commands are idempotent and print what they did. Every service-manager command
//! goes through a `Runner`, which the binary tests replace with a log.

mod flow;
pub mod launchd;
pub mod runner;
pub mod systemd;

use std::path::{Path, PathBuf};

use color_eyre::eyre::{WrapErr, bail, eyre};
use rail_core::paths::Paths;

use crate::cmd::account::report;
use crate::cmd::text::escape_field;

pub use flow::{install, uninstall};

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
    /// A command whose output describes the unit's state, shown when its daemon does not start.
    pub status: Option<Cmd>,
    /// A command that prints the pid of the process the manager runs for the unit (0: none).
    pub main_pid: Option<Cmd>,
    /// What to look at, or run, when the unit's daemon does not start.
    pub start_hint: String,
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

pub(crate) fn cmd(args: &[&str]) -> Cmd {
    args.iter().map(|a| (*a).to_string()).collect()
}

/// `path` for the terminal: control and invisible characters escaped.
pub(crate) fn shown(path: &Path) -> String {
    escape_field(&path.to_string_lossy(), usize::MAX)
}

/// `path` as text for a unit, refused when it is not UTF-8 or holds a control character
/// (a newline would end the setting and start another).
pub(crate) fn text_of(path: &Path) -> color_eyre::Result<String> {
    match path.to_str() {
        Some(text) if !text.chars().any(char::is_control) => Ok(text.to_string()),
        _ => bail!(
            "{} cannot go into a login unit (not UTF-8, or a control character)",
            shown(path)
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

/// `HOME` (required, absolute) and `XDG_CONFIG_HOME` (when absolute, else `~/.config`).
fn homes() -> color_eyre::Result<(PathBuf, PathBuf)> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|h| h.is_absolute())
        .ok_or_else(|| {
            eyre!("HOME is not set to an absolute path, so Rail cannot tell where login units go")
        })?;
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    Ok((home, config_home))
}

pub fn setup(paths: &Paths) -> color_eyre::Result<Setup> {
    let (home, config_home) = homes()?;
    setup_in(paths, home, config_home)
}

/// `setup` for a given `HOME` and config home.
fn setup_in(paths: &Paths, home: PathBuf, config_home: PathBuf) -> color_eyre::Result<Setup> {
    let exe = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .wrap_err("finding the rail binary")?;
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
        uid: current_uid(),
    })
}

/// The uid this process runs as: the user `rail daemon install` installs for, whose launchd
/// domain (`gui/<uid>`) the agent goes in. Not the owner of `HOME`, which can be another user
/// (`sudo -E`, a shared or root-owned home).
fn current_uid() -> u32 {
    rustix::process::getuid().as_raw()
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

#[cfg(test)]
mod tests;
