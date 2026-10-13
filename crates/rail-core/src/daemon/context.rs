//! What every handler runs against: the paths, the shared HTTP client, the browser opener and
//! the daemon's in-memory state.

use std::sync::{Arc, Mutex, PoisonError};

use url::Url;

use super::pending::PendingTable;
use crate::auth::{ItemIdCache, http_client};
use crate::ops::{BrowserEnv, OpError};
use crate::paths::Paths;
use crate::sync::SyncManager;

/// Starts the browser on a URL. Injected so tests never start a real one; the binary passes
/// its opener, which honours `RAIL_TEST_OPENER` in debug builds.
/// It gets the display and browser of the client that began the login.
pub type Opener = Arc<dyn Fn(&Url, &BrowserEnv) -> std::io::Result<()> + Send + Sync>;

pub struct DaemonContext {
    pub paths: Paths,
    /// Token requests and preset fetches: timeouts set, redirects off.
    pub http: reqwest::Client,
    pub opener: Opener,
    /// A token endpoint used instead of the provider's: a debug-build test hook, compiled out of
    /// release builds so nothing can redirect a real login.
    #[cfg(debug_assertions)]
    token_url: Option<String>,
    /// The Gmail API root used instead of Google's: a debug-build test hook like `token_url`.
    #[cfg(debug_assertions)]
    gmail_url: Option<String>,
    /// The Graph API root used instead of Microsoft's: a debug-build test hook like `token_url`.
    #[cfg(debug_assertions)]
    graph_url: Option<String>,
    /// One sync engine per configured account, their writer and status board.
    pub(crate) sync: SyncManager,
    /// 1Password item ids, kept for the daemon's lifetime.
    pub(crate) op_ids: ItemIdCache,
    /// Held by every handler that writes config.toml or moves tokens, so two front ends can
    /// never interleave their writes.
    config_lock: Mutex<()>,
    /// Logins and consents waiting on the user.
    pub(crate) pending: PendingTable,
    /// Tests hold a save at its start to prove a disconnect cannot stop it.
    #[cfg(any(test, feature = "test-gates"))]
    pub save_gate: Arc<SaveGate>,
    /// Tests hold begins after their loopback is bound to prove the cap holds under a burst.
    #[cfg(any(test, feature = "test-gates"))]
    pub begin_gate: Arc<BeginGate>,
}

/// A test hook: when armed, every begin counts itself in `reached` and waits for `open`.
#[cfg(any(test, feature = "test-gates"))]
pub struct BeginGate {
    pub armed: std::sync::atomic::AtomicBool,
    pub reached: tokio::sync::Semaphore,
    pub open: tokio::sync::watch::Sender<bool>,
    /// The login URL of every begin that reached the gate.
    pub urls: Mutex<Vec<Url>>,
}

#[cfg(any(test, feature = "test-gates"))]
impl Default for BeginGate {
    fn default() -> Self {
        Self {
            armed: std::sync::atomic::AtomicBool::new(false),
            reached: tokio::sync::Semaphore::new(0),
            open: tokio::sync::watch::Sender::new(false),
            urls: Mutex::new(Vec::new()),
        }
    }
}

#[cfg(any(test, feature = "test-gates"))]
impl BeginGate {
    pub(crate) async fn pass(&self, url: &Url) {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            self.urls
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(url.clone());
            self.reached.add_permits(1);
            let mut open = self.open.subscribe();
            let _ = open.wait_for(|open| *open).await;
        }
    }
}

/// A test hook: when armed, every save signals `reached` and waits for `release`.
#[cfg(any(test, feature = "test-gates"))]
#[derive(Default)]
pub struct SaveGate {
    pub armed: std::sync::atomic::AtomicBool,
    pub reached: tokio::sync::Notify,
    pub release: tokio::sync::Notify,
}

#[cfg(any(test, feature = "test-gates"))]
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
            #[cfg(debug_assertions)]
            gmail_url: None,
            #[cfg(debug_assertions)]
            graph_url: None,
            sync: SyncManager::new(Arc::new(crate::sync::providers::provider_for)),
            op_ids: ItemIdCache::default(),
            config_lock: Mutex::new(()),
            pending: PendingTable::default(),
            #[cfg(any(test, feature = "test-gates"))]
            save_gate: Arc::new(SaveGate::default()),
            #[cfg(any(test, feature = "test-gates"))]
            begin_gate: Arc::new(BeginGate::default()),
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

    /// Replace the Gmail API root. Honoured in debug builds only.
    #[cfg_attr(not(debug_assertions), allow(unused_mut, unused_variables))]
    pub fn with_gmail_url(mut self, gmail_url: Option<String>) -> Self {
        #[cfg(debug_assertions)]
        {
            self.gmail_url = gmail_url;
        }
        self
    }

    /// The Gmail API root override, if the hook is set and this is a debug build.
    pub(crate) fn gmail_url(&self) -> Option<&str> {
        #[cfg(debug_assertions)]
        {
            self.gmail_url.as_deref()
        }
        #[cfg(not(debug_assertions))]
        {
            None
        }
    }

    /// Replace the Graph API root. Honoured in debug builds only.
    #[cfg_attr(not(debug_assertions), allow(unused_mut, unused_variables))]
    pub fn with_graph_url(mut self, graph_url: Option<String>) -> Self {
        #[cfg(debug_assertions)]
        {
            self.graph_url = graph_url;
        }
        self
    }

    /// The Graph API root override, if the hook is set and this is a debug build.
    pub(crate) fn graph_url(&self) -> Option<&str> {
        #[cfg(debug_assertions)]
        {
            self.graph_url.as_deref()
        }
        #[cfg(not(debug_assertions))]
        {
            None
        }
    }

    /// Build every account's provider with `factory` instead of Gmail's (tests).
    #[cfg(any(test, feature = "test-gates"))]
    pub fn with_provider_factory(mut self, factory: crate::sync::ProviderFactory) -> Self {
        self.sync = SyncManager::new(factory);
        self
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
