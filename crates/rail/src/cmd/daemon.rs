//! `rail daemon run|status|stop`.

use std::process::ExitCode;
use std::sync::Arc;

use color_eyre::eyre::eyre;
use rail_core::daemon::{
    DAEMON_EXIT_TIMEOUT, DaemonClient, DaemonContext, IDLE_TIMEOUT, Opener, ServeConfig, Server,
    wait_for_daemon_exit,
};
use rail_core::ops::op::{DaemonInfoOp, DaemonStop};
use rail_core::ops::{DaemonInfo, DaemonInfoRequest, DaemonStopRequest, ErrorCode, OpError};
use rail_core::paths::Paths;
use tokio::signal::unix::{SignalKind, signal};

use crate::cmd::account::report;
use crate::{browser, daemon_link, test_hooks};

/// Serve until stopped, idle, or SIGINT or SIGTERM (which shut down like `daemon.stop`: saves
/// in flight get their grace, then the socket goes and the lock is released; exit 0). The
/// debug-only test hooks for the browser opener, the token endpoint, the Gmail API root and
/// the idle timeout (and its rule for configured accounts) reach the daemon here. Nothing is
/// printed: the daemon logs to `rail.log`, including why it could not start, since its
/// stderr goes nowhere.
pub async fn run(paths: &Paths) -> color_eyre::Result<ExitCode> {
    serve(paths).await.map_err(|error| {
        tracing::error!(error = %error.message, "the daemon could not start or stopped on an error");
        report(error)
    })?;
    Ok(ExitCode::SUCCESS)
}

async fn serve(paths: &Paths) -> Result<(), OpError> {
    let io = |e: std::io::Error| OpError::io(ErrorCode::Daemon, "listening for signals", &e);
    let mut interrupt = signal(SignalKind::interrupt()).map_err(io)?;
    let mut terminate = signal(SignalKind::terminate()).map_err(io)?;
    let signalled = async move {
        tokio::select! {
            _ = interrupt.recv() => {}
            _ = terminate.recv() => {}
        }
    };
    let opener: Opener = Arc::new(browser::launch);
    let ctx = DaemonContext::new(paths.clone(), opener)?
        .with_token_url(test_hooks::token_url())
        .with_gmail_url(test_hooks::gmail_url());
    let config = ServeConfig {
        idle: test_hooks::idle().unwrap_or(IDLE_TIMEOUT),
        idle_with_accounts: test_hooks::idle_with_accounts(),
        ..ServeConfig::default()
    };
    Server::bind(ctx, config).await?.run_until(signalled).await
}

/// Exit 0 when a daemon answers, 1 when none runs. Never starts one.
pub async fn status(paths: &Paths) -> color_eyre::Result<ExitCode> {
    let Some(client) = daemon_link::connector(paths)
        .connect_existing()
        .await
        .map_err(report)?
    else {
        println!(
            "rail daemon is not running (socket {})",
            paths.socket_file().display()
        );
        return Ok(ExitCode::from(1));
    };
    let info = client
        .call::<DaemonInfoOp>(&DaemonInfoRequest {})
        .await
        .map_err(report)?;
    println!("{}", describe(&info));
    Ok(ExitCode::SUCCESS)
}

pub fn describe(info: &DaemonInfo) -> String {
    let build = info
        .build
        .map_or_else(String::new, |build| format!(" (build {build})"));
    format!(
        "rail daemon running: pid {}, Rail {}{build}, protocol {}, {} pending, {} connected (including this one), socket {}",
        info.pid,
        info.version,
        info.protocol,
        info.pending,
        info.clients,
        info.socket.display()
    )
}

/// Stop a running daemon and wait until it has exited (it releases `daemon.lock` last, after
/// its socket is gone); succeed quietly when none runs.
pub async fn stop(paths: &Paths) -> color_eyre::Result<ExitCode> {
    let Some(client) = daemon_link::connector(paths)
        .connect_existing()
        .await
        .map_err(report)?
    else {
        println!("rail daemon is not running");
        return Ok(ExitCode::SUCCESS);
    };
    let pid = client.daemon().pid;
    ask_to_stop(client).await?;
    let lock = paths.daemon_lock_file();
    tokio::task::spawn_blocking(move || wait_for_daemon_exit(&lock, DAEMON_EXIT_TIMEOUT))
        .await?
        .map_err(|e| match e.code {
            ErrorCode::Timeout => not_stopped(pid),
            _ => report(e),
        })?;
    println!("rail daemon stopped");
    Ok(ExitCode::SUCCESS)
}

/// Send `daemon.stop`. The daemon may close the connection before it answers; that is a
/// daemon that is stopping, not a failure.
async fn ask_to_stop(client: DaemonClient) -> color_eyre::Result<()> {
    match client.call::<DaemonStop>(&DaemonStopRequest {}).await {
        Ok(_) => Ok(()),
        Err(e) if e.code == ErrorCode::Daemon => Ok(()),
        Err(e) => Err(report(e)),
    }
}

fn not_stopped(pid: u32) -> color_eyre::Report {
    eyre!(
        "the rail daemon (pid {pid}) was asked to stop but has not exited after {} seconds\nhint: it may still be saving an account; run `rail daemon status`, or end process {pid} by hand",
        DAEMON_EXIT_TIMEOUT.as_secs()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_names_the_pid_version_load_and_socket() {
        let info = DaemonInfo {
            pid: 42,
            version: "0.1.0".into(),
            build: Some(1760270000),
            protocol: 1,
            socket: "/run/user/1000/rail/rail.sock".into(),
            started_at: 0,
            clients: 3,
            pending: 2,
        };
        assert_eq!(
            describe(&info),
            "rail daemon running: pid 42, Rail 0.1.0 (build 1760270000), protocol 1, 2 pending, 3 connected (including this one), socket /run/user/1000/rail/rail.sock"
        );
    }

    #[test]
    fn describe_leaves_the_build_out_when_the_daemon_sent_none() {
        let info = DaemonInfo {
            pid: 42,
            version: "0.1.0".into(),
            build: None,
            protocol: 1,
            socket: "/run/rail.sock".into(),
            started_at: 0,
            clients: 1,
            pending: 0,
        };
        assert!(describe(&info).contains("Rail 0.1.0, protocol 1"));
    }

    #[test]
    fn a_daemon_that_does_not_exit_is_named_with_a_hint() {
        let text = not_stopped(77).to_string();
        assert!(text.contains("pid 77"), "{text}");
        assert!(text.contains("35 seconds"), "{text}");
        assert!(text.contains("hint: "), "{text}");
    }
}
