//! The account pipeline the daemon runs for every front end: resolve the client secret,
//! validate the new account against config, log in, and persist tokens, then the store row,
//! then config.toml last.

pub mod add;
pub mod client_secret;
pub mod login;
pub mod remove;

pub use add::{
    PreparedAdd, persist_new, prepare_add, prepare_and_probe, prepare_request, probe_secrets,
};
pub use client_secret::{MAX_CLIENT_FILE_BYTES, ResolvedSecret, resolve_client_secret};
pub use login::{
    account_for_login, endpoints_for_login, find_account, login_secret, persist_login, run_login,
    saved_account,
};
pub use remove::{REMOVE_NEEDS_CONFIRMATION, removal_plan, remove_account, remove_confirmed};

use crate::ops::OpError;

/// Prefix an error's message with what Rail was doing, keeping its code, class and hint.
pub(crate) fn context(what: &str, error: impl Into<OpError>) -> OpError {
    let mut error = error.into();
    error.message = format!("{what}: {}", error.message);
    error
}

/// True when a preset source names a URL (`http://` is still refused later, as not https).
pub(crate) fn is_url(source: &str) -> bool {
    source.starts_with("https://") || source.starts_with("http://")
}

/// The daemon's working directory is arbitrary, so a local path a client sends must be absolute.
pub(crate) fn require_absolute(path: &std::path::Path) -> Result<(), OpError> {
    if path.is_absolute() {
        Ok(())
    } else {
        Err(OpError::invalid_input(
            "use an absolute path: the daemon's working directory is not yours",
        ))
    }
}

/// Run synchronous file, keyring or `op` work on the blocking pool.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, OpError> + Send + 'static,
) -> Result<T, OpError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| OpError::internal("a blocking task failed").with_detail(e.to_string()))?
}
