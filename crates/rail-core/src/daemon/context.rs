//! What every handler runs against: the paths, the shared HTTP client, the browser opener and
//! the daemon's in-memory state.

use std::sync::{Arc, Mutex, PoisonError};

use url::Url;

use crate::auth::{ItemIdCache, http_client};
use crate::ops::OpError;
use crate::paths::Paths;

/// Starts the browser on a URL. Injected so tests never start a real one; the binary passes
/// its opener, which honours `RAIL_TEST_OPENER` in debug builds.
pub type Opener = Arc<dyn Fn(&Url) -> std::io::Result<()> + Send + Sync>;

pub struct DaemonContext {
    pub paths: Paths,
    /// Token requests and preset fetches: timeouts set, redirects off.
    pub http: reqwest::Client,
    pub opener: Opener,
    /// A token endpoint used instead of the provider's (the binary's debug-only test hook).
    pub token_url: Option<String>,
    /// 1Password item ids, kept for the daemon's lifetime.
    pub(crate) op_ids: ItemIdCache,
    /// Held by every handler that writes config.toml or moves tokens, so two front ends can
    /// never interleave their writes.
    config_lock: Mutex<()>,
}

impl DaemonContext {
    pub fn new(paths: Paths, opener: Opener) -> Result<Self, OpError> {
        Ok(Self {
            paths,
            http: http_client()?,
            opener,
            token_url: None,
            op_ids: ItemIdCache::default(),
            config_lock: Mutex::new(()),
        })
    }

    pub fn with_token_url(mut self, token_url: Option<String>) -> Self {
        self.token_url = token_url;
        self
    }

    /// Logins and consents waiting on a browser or saving. None exist yet: the pending
    /// operations arrive with `account.add.begin`.
    pub(crate) fn pending_count(&self) -> usize {
        0
    }

    /// Run synchronous file, store or secret-store work on the blocking pool. Once started it
    /// runs to completion, even if the request that asked for it is dropped.
    pub(crate) async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> Result<T, OpError> + Send + 'static,
    ) -> Result<T, OpError> {
        tokio::task::spawn_blocking(work)
            .await
            .map_err(|e| OpError::internal(format!("blocking task failed: {e}")))?
    }

    /// `blocking`, holding the config lock for the whole of `work`.
    pub(crate) async fn write_locked<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(&DaemonContext) -> Result<T, OpError> + Send + 'static,
    ) -> Result<T, OpError> {
        let ctx = self.clone();
        self.blocking(move || {
            let _guard = ctx
                .config_lock
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            work(&ctx)
        })
        .await
    }
}
