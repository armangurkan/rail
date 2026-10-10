//! How the binary reaches its daemon: the `rail-core` connector, with a spawn that starts this
//! same binary as `rail daemon run`, detached from the terminal.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::Arc;

use rail_core::daemon::Connector;
use rail_core::ops::{ErrorCode, OpError};
use rail_core::paths::Paths;

use crate::browser;

pub fn connector(paths: &Paths) -> Connector {
    let dirs = paths.clone();
    Connector::new(paths.clone(), Arc::new(move || spawn_daemon(&dirs)))
}

/// Start `rail daemon run` in its own process group (so Ctrl-C in the terminal does not reach
/// it) with stdio closed (so nothing it does can write to the terminal or hold a pipe open),
/// in `/` (so it pins no directory). Every `RAIL_*_DIR` is set to this client's resolved,
/// absolute directories, so the daemon works on exactly the files this client means. The rest
/// of the environment (display, browser, 1Password session, `PATH`) is inherited. A thread
/// reaps it if it exits while this process still runs.
fn spawn_daemon(paths: &Paths) -> Result<(), OpError> {
    let exe = std::env::current_exe()
        .map_err(|e| OpError::io(ErrorCode::Daemon, "finding the rail binary", &e))?;
    let child = Command::new(exe)
        .args(["daemon", "run"])
        .current_dir("/")
        .env("RAIL_CONFIG_DIR", &paths.config_dir)
        .env("RAIL_DATA_DIR", &paths.data_dir)
        .env("RAIL_CACHE_DIR", &paths.cache_dir)
        .env("RAIL_STATE_DIR", &paths.state_dir)
        .env("RAIL_RUNTIME_DIR", &paths.runtime_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| OpError::io(ErrorCode::Daemon, "starting the daemon", &e))?;
    browser::reap(child);
    Ok(())
}
