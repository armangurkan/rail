//! File logging under the state dir. Never logs tokens or message bodies.

use rail_core::paths::Paths;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

/// Starts logging to `rail.log` in the state dir. The returned guard must live
/// until exit so buffered lines are flushed. If the log directory cannot be
/// prepared, one warning goes to stderr and the CLI runs without file logs.
pub fn init(paths: &Paths) -> Option<WorkerGuard> {
    let dir = paths.log_dir();
    if let Err(e) = std::fs::create_dir_all(dir) {
        eprintln!(
            "rail: warning: cannot create log directory {}: {e}; continuing without file logs",
            dir.display()
        );
        return None;
    }
    let file = tracing_appender::rolling::never(dir, "rail.log");
    let (writer, guard) = tracing_appender::non_blocking(file);
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(writer)
        .with_ansi(false)
        .init();
    Some(guard)
}
