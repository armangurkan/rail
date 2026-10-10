//! File logging under the state dir. Never logs tokens or message bodies.

use rail_core::paths::Paths;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

const LOG_FILE: &str = "rail.log";

/// Starts logging to `rail.log` in the state dir. The returned guard must live
/// until exit so buffered lines are flushed. If logging cannot be set up, one
/// warning goes to stderr and the CLI runs without file logs. Never panics.
/// With `mcp` (`rail mcp`), rmcp logs only errors: its warnings about tool arguments that do
/// not parse quote the agent's value, which may be a pasted secret.
pub fn init(paths: &Paths, mcp: bool) -> Option<WorkerGuard> {
    let dir = paths.log_dir();
    match try_init(dir, filter(mcp)) {
        Ok(guard) => Some(guard),
        Err(e) => {
            eprintln!(
                "rail: warning: cannot set up logging in {}: {e}; continuing without file logs",
                dir.display()
            );
            None
        }
    }
}

/// `RUST_LOG`, or `info`; for `rail mcp` always with `rmcp=error` on top.
fn filter(mcp: bool) -> EnvFilter {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    if !mcp {
        return filter;
    }
    match "rmcp=error".parse() {
        Ok(directive) => filter.add_directive(directive),
        Err(_) => filter,
    }
}

fn try_init(dir: &std::path::Path, filter: EnvFilter) -> Result<WorkerGuard, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix(LOG_FILE)
        .build(dir)
        .map_err(|e| e.to_string())?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .try_init()
        .map_err(|e| e.to_string())?;
    Ok(guard)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_returns_none_when_log_file_is_a_directory() {
        let dir = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(dir.path());
        std::fs::create_dir_all(paths.log_dir().join(LOG_FILE)).unwrap();
        assert!(init(&paths, false).is_none());
    }

    #[test]
    fn rail_mcp_logs_only_rmcp_errors() {
        assert!(filter(true).to_string().contains("rmcp=error"));
        assert!(!filter(false).to_string().contains("rmcp"));
    }
}
