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
pub use remove::{removal_summary, remove_account};

use crate::ops::OpError;

/// Prefix an error's message with what Rail was doing, keeping its code, class and hint.
pub(crate) fn context(what: &str, error: impl Into<OpError>) -> OpError {
    let mut error = error.into();
    error.message = format!("{what}: {}", error.message);
    error
}

/// Run synchronous file, keyring or `op` work on the blocking pool.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, OpError> + Send + 'static,
) -> Result<T, OpError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| OpError::internal("a blocking task failed").with_detail(e.to_string()))?
}
