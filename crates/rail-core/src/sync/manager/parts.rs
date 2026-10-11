//! The manager's own bookkeeping: what runs, what failed and when to try again, and the small
//! pieces the reconcile and the checks share.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::ENGINE_STOP;
use crate::auth::unix_now;
use crate::config::{
    self, AccountConfig, Config, ConfigError, OnePasswordConfig, SecretsBackend, SyncSettings,
};
use crate::error::ErrorClass;
use crate::ops::{AccountSyncStatus, ErrorCode, OpError, SyncErrorInfo, SyncPhase};
use crate::provider::Provider;
use crate::provider::http::RetryObserver;
use crate::store::{Store, SyncStateRow};
use crate::sync::status::StatusBoard;
use crate::sync::writer::{WriterHandle, WriterTask};

/// How long after each failed start (or engine crash) an account is tried again: 1, 5, then
/// every 15 minutes.
pub(super) const START_RETRIES: [Duration; 3] = [
    Duration::from_secs(60),
    Duration::from_secs(300),
    Duration::from_secs(900),
];

pub(super) fn retry_delay(attempts: u32) -> Duration {
    let index = usize::try_from(attempts.saturating_sub(1)).unwrap_or(usize::MAX);
    START_RETRIES[index.min(START_RETRIES.len() - 1)]
}

/// Clears the `checking` flag however the check ends, so a panic cannot stop the checks.
pub(super) struct Checked<'a>(pub(super) &'a AtomicBool);

impl Drop for Checked<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// What an engine was started with; a change restarts it.
#[derive(Clone, PartialEq)]
pub(super) struct EngineKey {
    account: AccountConfig,
    settings: SyncSettings,
    secrets: SecretsBackend,
    onepassword: Option<OnePasswordConfig>,
}

pub(super) fn key(config: &Config, account: &AccountConfig) -> EngineKey {
    EngineKey {
        account: account.clone(),
        settings: SyncSettings::for_account(&config.general, account),
        secrets: config.general.secrets,
        onepassword: config.onepassword.clone(),
    }
}

pub(super) struct Running {
    pub(super) key: EngineKey,
    pub(super) name: String,
    pub(super) account_id: i64,
    pub(super) wake: Arc<Notify>,
    pub(super) stop: watch::Sender<bool>,
    pub(super) task: JoinHandle<()>,
    pub(super) provider: Arc<dyn Provider>,
}

/// An account with no engine because its start failed (or its engine ended): why, and when
/// it is tried again.
pub(super) struct Failed {
    pub(super) error: OpError,
    pub(super) retry_at: Instant,
}

/// Everything a new engine needs that is slow to get (the store rows, the provider, which may
/// run `op read`), gathered without the engines lock.
pub(super) struct Prepared {
    pub(super) account_id: i64,
    pub(super) state: SyncStateRow,
    pub(super) reader: Store,
    pub(super) provider: Arc<dyn Provider>,
}

#[derive(Default)]
pub(super) struct Engines {
    pub(super) running: HashMap<String, Running>,
    pub(super) writer: Option<(WriterHandle, WriterTask)>,
    /// When the current writer started.
    pub(super) writer_started: Option<Instant>,
    /// Accounts (by lowercase address) without an engine because it failed.
    pub(super) failed: HashMap<String, Failed>,
    /// Failed starts and crashes per account, for the backoff; forgotten when it goes.
    pub(super) attempts: HashMap<String, u32>,
    pub(super) writer_restarts: u32,
}

impl Engines {
    /// Record that `address` has no engine because of `error`, and when to try again.
    pub(super) fn record_failure(&mut self, address: &str, error: OpError) {
        let attempts = self.attempts.entry(address.to_string()).or_insert(0);
        *attempts += 1;
        let retry_at = Instant::now() + retry_delay(*attempts);
        self.failed
            .insert(address.to_string(), Failed { error, retry_at });
    }

    pub(super) fn retry_due(&self) -> bool {
        let now = Instant::now();
        self.failed.values().any(|f| f.retry_at <= now)
    }
}

pub(super) fn not_syncing(who: &str) -> OpError {
    OpError::not_found(format!("no account named {who:?} is configured for sync"))
        .with_hint("`rail account list` shows the configured accounts")
}

pub(super) fn unsupported() -> OpError {
    OpError::invalid_input("Microsoft 365 sync arrives in a later phase of Rail")
}

pub(super) fn not_running() -> OpError {
    OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Transient,
        "this account's sync is not running yet",
    )
    .with_hint("`rail doctor` and the daemon's log say why")
}

pub(super) fn shutting_down() -> OpError {
    OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Transient,
        "the daemon is shutting down",
    )
}

pub(super) fn writer_gone(failure: String) -> OpError {
    OpError::new(ErrorCode::Store, ErrorClass::Permanent, failure)
}

pub(super) fn engine_ended() -> OpError {
    OpError::new(
        ErrorCode::Internal,
        ErrorClass::Transient,
        "this account's sync stopped unexpectedly; it starts again shortly",
    )
}

pub(super) fn named(status: &AccountSyncStatus, who: &str) -> bool {
    status.name.eq_ignore_ascii_case(who) || status.address.eq_ignore_ascii_case(who)
}

pub(super) async fn stop_engine(running: Running) {
    let _ = running.stop.send(true);
    let mut task = running.task;
    if tokio::time::timeout(ENGINE_STOP, &mut task).await.is_err() {
        tracing::warn!(account = %running.name, "an engine did not stop in time; aborting it");
        task.abort();
    }
}

/// Drop the manager's handle and wait, at most `grace`, for the writer to commit what it has.
pub(super) async fn finish_writer(writer: Option<(WriterHandle, WriterTask)>, grace: Duration) {
    if let Some((handle, task)) = writer {
        drop(handle);
        if !task.finish(grace).await {
            tracing::warn!("the store writer did not end cleanly");
        }
    }
}

pub(super) fn retry_observer(board: StatusBoard, address: String) -> RetryObserver {
    Arc::new(move |wait: Duration| {
        let at = unix_now() + i64::try_from(wait.as_secs()).unwrap_or(0);
        board.update(&address, |s| {
            s.phase = SyncPhase::Retrying;
            s.next_attempt_at = Some(at);
        });
    })
}

/// The status an engine starts from: what the store remembers.
pub(super) fn initial_status(account: &AccountConfig, state: &SyncStateRow) -> AccountSyncStatus {
    let mut status = AccountSyncStatus::new(&account.name, &account.address, account.kind);
    status.needs_auth = state.needs_auth;
    if state.needs_auth {
        status.phase = SyncPhase::NeedsAuth;
    }
    status.last_success_at = state.last_sync_at;
    if let (Some(code), Some(at)) = (&state.last_error_code, state.last_error_at) {
        status.last_error = Some(SyncErrorInfo {
            code: code.clone(),
            message: "the last sync pass failed".to_string(),
            at,
            detail: None,
        });
    }
    status
}

/// Mark a status stopped by `error` (`code` says what kind of stop).
pub(super) fn stopped_by(status: &mut AccountSyncStatus, code: &str, error: &OpError) {
    status.phase = SyncPhase::Stopped;
    status.pass_running = false;
    status.last_pass_failed = true;
    status.next_attempt_at = None;
    status.last_error = Some(SyncErrorInfo {
        code: code.to_string(),
        message: error.message.clone(),
        at: unix_now(),
        detail: error.detail.clone(),
    });
}

/// The status of an account whose engine could not start.
pub(super) fn failed_status(account: &AccountConfig, error: &OpError) -> AccountSyncStatus {
    let mut status = AccountSyncStatus::new(&account.name, &account.address, account.kind);
    stopped_by(&mut status, "start", error);
    status
}

/// The config file's modification time and its contents; a missing file is no accounts.
pub(super) fn read_config(path: &std::path::Path) -> (Option<SystemTime>, Result<Config, OpError>) {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let config = match config::load(path) {
        Ok(config) => Ok(config),
        Err(ConfigError::Missing { .. }) => Ok(Config::default()),
        Err(error) => Err(error.into()),
    };
    (mtime, config)
}

/// The writer's restart count once it has run for `healthy_for`: a writer that stayed up an
/// hour starts the count again.
pub(super) fn restarts_after(restarts: u32, healthy_for: Option<Duration>) -> u32 {
    if healthy_for.is_some_and(|d| d >= super::WRITER_HEALTHY) {
        0
    } else {
        restarts
    }
}
