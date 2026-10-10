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
    Connector::new(paths.clone(), Arc::new(spawn_daemon))
}

/// Start `rail daemon run` in its own process group (so Ctrl-C in the terminal does not reach
/// it) with stdio closed (so nothing it does can write to the terminal or hold a pipe open),
/// in `/` (so it pins no directory; it refuses relative paths anyway). A thread reaps it if it
/// exits while this process still runs.
fn spawn_daemon() -> Result<(), OpError> {
    let exe = std::env::current_exe()
        .map_err(|e| OpError::io(ErrorCode::Daemon, "finding the rail binary", &e))?;
    let child = Command::new(exe)
        .args(["daemon", "run"])
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| OpError::io(ErrorCode::Daemon, "starting the daemon", &e))?;
    browser::reap(child);
    Ok(())
}
