//! The steps of `rail daemon install` and `uninstall`.

use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::process::ExitCode;
use std::time::Duration;

use color_eyre::eyre::{WrapErr, bail};
use rail_core::paths::Paths;

use super::runner::{self, Runner, failure, run_strict, run_tolerant};
use super::{Unit, setup, shown, unit};
use crate::cmd::account::{outside, report};
use crate::daemon_link;

/// How long install waits for the unit's daemon to answer once the unit is started.
pub const START_WAIT: Duration = Duration::from_secs(10);

/// How often install looks for the unit's daemon while it waits.
const START_POLL: Duration = Duration::from_millis(200);

/// Unit files are not secret: other tools (and `systemctl cat`) read them.
const UNIT_MODE: u32 = 0o644;

pub async fn install(paths: &Paths) -> color_eyre::Result<ExitCode> {
    let unit = unit(&setup(paths)?)?;
    // The test log starts nothing, so there is no daemon to wait for.
    let wait = (!runner::logging_only()).then_some(START_WAIT);
    install_with(paths, &unit, &*runner::runner(), wait).await
}

pub async fn uninstall(paths: &Paths) -> color_eyre::Result<ExitCode> {
    let unit = unit(&setup(paths)?)?;
    if !unit.path.exists() {
        println!("Rail's login unit is not installed; nothing to remove.");
        return Ok(ExitCode::SUCCESS);
    }
    remove(&unit, &*runner::runner())?;
    Ok(ExitCode::SUCCESS)
}

/// Check the manager, stop a running daemon, put the unit in place and, given `wait`, check
/// that the unit's own daemon answers.
pub(super) async fn install_with(
    paths: &Paths,
    unit: &Unit,
    runner: &dyn Runner,
    wait: Option<Duration>,
) -> color_eyre::Result<ExitCode> {
    require_manager(unit, runner)?;
    stop_running_daemon(paths).await?;
    put_in_place(unit, runner)?;
    match wait {
        None => {
            println!("Rail's daemon now starts at login ({}).", unit.name);
            Ok(ExitCode::SUCCESS)
        }
        Some(limit) => check_started(paths, unit, runner, limit).await,
    }
}

/// `Ok` when the unit's service manager answers (always, without a probe); `Err` says why not,
/// with the tool's own words as outside text.
pub(super) fn manager(unit: &Unit, runner: &dyn Runner) -> Result<(), String> {
    let Some(probe) = &unit.probe else {
        return Ok(());
    };
    let result = runner.run(probe);
    let reason = match &result {
        Ok(outcome) if outcome.ok => return Ok(()),
        Ok(_) => "no user service manager answers",
        Err(_) => "the service manager's command cannot be run here",
    };
    Err(format!("{reason}: {}", failure(&probe.join(" "), &result)))
}

/// Refuse before anything changes when there is no service manager to enable the unit with
/// (some containers have no `systemd --user`; some systems no `systemctl`).
pub(super) fn require_manager(unit: &Unit, runner: &dyn Runner) -> color_eyre::Result<()> {
    if let Err(why) = manager(unit, runner) {
        bail!(
            "nothing was installed, because {why}\nhint: without a login unit, Rail still starts its daemon on demand"
        );
    }
    Ok(())
}

/// Write the unit (unless it is already exactly this) and enable it. If enabling fails, the
/// previous unit is put back (or the new one removed), so nothing is left half-installed.
pub(super) fn put_in_place(unit: &Unit, runner: &dyn Runner) -> color_eyre::Result<()> {
    run_tolerant(&unit.before_install, runner);
    let previous = std::fs::read_to_string(&unit.path).ok();
    if previous.as_deref() == Some(unit.text.as_str()) {
        println!("{} is already up to date", shown(&unit.path));
    } else {
        write_unit_file(&unit.path, &unit.text)
            .wrap_err_with(|| format!("writing {}", shown(&unit.path)))?;
        println!("Wrote {}", shown(&unit.path));
    }
    if let Err(error) = run_strict(&unit.after_install, runner) {
        roll_back(unit, runner, previous.as_deref());
        return Err(error);
    }
    Ok(())
}

/// Undo a failed install: the previous file back (or the new one gone), then the manager told.
/// A failure here is printed; the install's own error is what the command returns.
fn roll_back(unit: &Unit, runner: &dyn Runner, previous: Option<&str>) {
    let path = shown(&unit.path);
    let undone = match previous {
        Some(text) => write_unit_file(&unit.path, text)
            .map(|()| format!("Put back the previous {path}, so nothing is left half-installed")),
        None => {
            run_tolerant(&unit.before_remove, runner);
            std::fs::remove_file(&unit.path)
                .map(|()| format!("Removed {path} again, so nothing is left half-installed"))
        }
    };
    match undone {
        Ok(done) => println!("{done}"),
        Err(error) => eprintln!(
            "could not undo the install of {path}\n  {}",
            outside(&error.to_string())
        ),
    }
    run_tolerant(&unit.after_remove, runner);
}

/// Stop and disable the unit, then remove its file (and an enable link left behind when no
/// service manager answers).
pub(super) fn remove(unit: &Unit, runner: &dyn Runner) -> color_eyre::Result<()> {
    let answers = match manager(unit, runner) {
        Ok(()) => true,
        Err(why) => {
            println!("Nothing to stop or disable, because {why}");
            false
        }
    };
    if answers {
        run_tolerant(&unit.before_remove, runner);
    }
    std::fs::remove_file(&unit.path).wrap_err_with(|| format!("removing {}", shown(&unit.path)))?;
    println!("Removed {}", shown(&unit.path));
    if let Some(marker) = &unit.enabled_marker
        && std::fs::symlink_metadata(marker).is_ok()
    {
        std::fs::remove_file(marker).wrap_err_with(|| format!("removing {}", shown(marker)))?;
        println!("Removed {}", shown(marker));
    }
    if answers {
        run_strict(&unit.after_remove, runner)?;
    }
    Ok(())
}

/// Write `text` to `path` atomically with mode 0644: a sibling temp file, synced, renamed over
/// it. A missing directory is created with the default mode; an existing one keeps its mode.
pub(super) fn write_unit_file(path: &Path, text: &str) -> std::io::Result<()> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(std::io::Error::other(
            "a unit path needs a directory and a name",
        ));
    };
    std::fs::create_dir_all(dir)?;
    let mut tmp_name = std::ffi::OsString::from(".");
    tmp_name.push(name);
    tmp_name.push(".tmp");
    let tmp = dir.join(tmp_name);
    let result = write_synced(&tmp, text).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    std::fs::File::open(dir)?.sync_all()
}

fn write_synced(path: &Path, text: &str) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(UNIT_MODE)
        .open(path)?;
    // The umask may have taken bits off; a unit is 0644 whatever it is.
    file.set_permissions(std::fs::Permissions::from_mode(UNIT_MODE))?;
    file.write_all(text.as_bytes())?;
    file.sync_all()
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

/// Wait for a daemon to answer; check it is the unit's. Exit 1 (the unit stays installed)
/// when none answers in time or another command's daemon answers instead.
pub(super) async fn check_started(
    paths: &Paths,
    unit: &Unit,
    runner: &dyn Runner,
    limit: Duration,
) -> color_eyre::Result<ExitCode> {
    let name = &unit.name;
    let pid = match wait_for_daemon(paths, limit).await {
        Ok(pid) => pid,
        Err(last) => {
            eprintln!(
                "Rail's login unit {name} is installed, but its daemon did not answer within {} seconds",
                limit.as_secs()
            );
            if let Some(error) = last {
                eprintln!("  {error}");
            }
            print_state(unit, runner);
            eprintln!("hint: {}", unit.start_hint);
            return Ok(ExitCode::from(1));
        }
    };
    if let Some(main) = unit_pid(unit, runner)
        && main != pid
    {
        eprintln!(
            "Rail's login unit {name} is installed, but the daemon that answers (pid {pid}) is not the one {name} runs: another command started it in between"
        );
        eprintln!("hint: run `rail daemon stop`; then {}", unit.start_hint);
        return Ok(ExitCode::from(1));
    }
    println!("Rail's daemon now starts at login ({name}); it is running now (pid {pid}).");
    Ok(ExitCode::SUCCESS)
}

/// The pid of the daemon that answers within `limit`, or the last connection error (Rail's
/// text) if one came.
async fn wait_for_daemon(paths: &Paths, limit: Duration) -> Result<u32, Option<String>> {
    let connector = daemon_link::connector(paths);
    let deadline = tokio::time::Instant::now() + limit;
    let mut last = None;
    loop {
        match connector.connect_existing().await {
            Ok(Some(client)) => return Ok(client.daemon().pid),
            Ok(None) => {}
            Err(error) => last = Some(report(error).to_string()),
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(last);
        }
        tokio::time::sleep(START_POLL).await;
    }
}

/// What the manager says about the unit, as outside text.
fn print_state(unit: &Unit, runner: &dyn Runner) {
    let Some(status) = &unit.status else {
        return;
    };
    let result = runner.run(status);
    match &result {
        Ok(outcome) => {
            let text = [outcome.stdout.as_str(), outcome.stderr.as_str()]
                .iter()
                .filter(|t| !t.is_empty())
                .copied()
                .collect::<Vec<_>>()
                .join("\n");
            eprintln!("`{}` says:\n  {}", status.join(" "), outside(&text));
        }
        Err(_) => eprintln!("{}", failure(&status.join(" "), &result)),
    }
}

/// The pid the manager runs for the unit, when it can say. A failure here only skips the
/// "is it the unit's daemon" check: the daemon did answer, which install already reports.
fn unit_pid(unit: &Unit, runner: &dyn Runner) -> Option<u32> {
    let command = unit.main_pid.as_ref()?;
    let outcome = runner.run(command).ok().filter(|o| o.ok)?;
    outcome.stdout.trim().parse().ok()
}
