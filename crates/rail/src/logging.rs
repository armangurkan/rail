//! File logging under the state dir. Never logs tokens or message bodies.

use rail_core::paths::Paths;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::EnvFilter;

const LOG_FILE: &str = "rail.log";

/// Starts logging to `rail.log` in the state dir. The returned guard must live
/// until exit so buffered lines are flushed. If logging cannot be set up, one
/// warning goes to stderr and the CLI runs without file logs. Never panics.
pub fn init(paths: &Paths) -> Option<WorkerGuard> {
    let dir = paths.log_dir();
    match try_init(dir) {
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

fn try_init(dir: &std::path::Path) -> Result<WorkerGuard, String> {
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let file = RollingFileAppender::builder()
        .rotation(Rotation::NEVER)
        .filename_prefix(LOG_FILE)
        .build(dir)
        .map_err(|e| e.to_string())?;
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
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
        assert!(init(&paths).is_none());
    }
}
