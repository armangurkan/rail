//! What every handler runs against: the paths, the shared HTTP client, the browser opener and
//! the daemon's in-memory state.

use std::sync::{Arc, Mutex, PoisonError};

use url::Url;

use super::pending::PendingTable;
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
    /// A token endpoint used instead of the provider's: a debug-build test hook, compiled out of
    /// release builds so nothing can redirect a real login.
    #[cfg(debug_assertions)]
    token_url: Option<String>,
    /// 1Password item ids, kept for the daemon's lifetime.
    pub(crate) op_ids: ItemIdCache,
    /// Held by every handler that writes config.toml or moves tokens, so two front ends can
    /// never interleave their writes.
    config_lock: Mutex<()>,
    /// Logins and consents waiting on the user.
    pub(crate) pending: PendingTable,
    /// Tests hold a save at its start to prove a disconnect cannot stop it.
    #[cfg(test)]
    pub(crate) save_gate: Arc<SaveGate>,
}

/// A test hook: when armed, every save signals `reached` and waits for `release`.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct SaveGate {
    pub(crate) armed: std::sync::atomic::AtomicBool,
    pub(crate) reached: tokio::sync::Notify,
    pub(crate) release: tokio::sync::Notify,
}

#[cfg(test)]
impl SaveGate {
    pub(crate) async fn pass(&self) {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            self.reached.notify_one();
            self.release.notified().await;
        }
    }
}

impl DaemonContext {
    pub fn new(paths: Paths, opener: Opener) -> Result<Self, OpError> {
        Ok(Self {
            paths,
            http: http_client()?,
            opener,
            #[cfg(debug_assertions)]
            token_url: None,
            op_ids: ItemIdCache::default(),
            config_lock: Mutex::new(()),
            pending: PendingTable::default(),
            #[cfg(test)]
            save_gate: Arc::new(SaveGate::default()),
        })
    }

    /// Replace the provider's token endpoint. Honoured in debug builds only.
    #[cfg_attr(not(debug_assertions), allow(unused_mut, unused_variables))]
    pub fn with_token_url(mut self, token_url: Option<String>) -> Self {
        #[cfg(debug_assertions)]
        {
            self.token_url = token_url;
        }
        self
    }

    /// The token endpoint override, if the hook is set and this is a debug build.
    pub(crate) fn token_url(&self) -> Option<&str> {
        #[cfg(debug_assertions)]
        {
            self.token_url.as_deref()
        }
        #[cfg(not(debug_assertions))]
        {
            None
        }
    }

    /// Logins and consents waiting on a browser or saving.
    pub(crate) fn pending_count(&self) -> usize {
        self.pending.active()
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
