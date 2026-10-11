//! The daemon's sync: one engine per configured Gmail account, the writer they share and the
//! status board, kept in step with `config.toml` (spec §3, "Daemon").

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use tokio::sync::{Notify, watch};
use tokio::task::JoinHandle;

use super::batch::{Batch, StateUpdate};
use super::engine::{Engine, EngineParts};
use super::status::{StatusBoard, Subscriber};
use super::writer::{WriterHandle, WriterTask, spawn_writer};
use crate::auth::{ItemIdCache, unix_now};
use crate::config::{
    self, AccountConfig, AccountKind, Config, ConfigError, OnePasswordConfig, SecretsBackend,
    SyncSettings,
};
use crate::daemon::DaemonContext;
use crate::error::ErrorClass;
use crate::ops::{
    AccountSyncStatus, DoctorReport, ErrorCode, OpError, SyncErrorInfo, SyncPhase, SyncTarget,
    Untrusted,
};
use crate::paths::Paths;
use crate::provider::Provider;
use crate::provider::http::RetryObserver;
use crate::store::{Store, SyncStateRow};

/// How often the daemon looks at `config.toml`'s modification time (and at the writer).
pub const CONFIG_CHECK: Duration = Duration::from_secs(2);
/// How long a stopping engine may take before its task is aborted.
pub const ENGINE_STOP: Duration = Duration::from_secs(5);
/// The longest a request waits for the daemon's first reconcile (an `op read` may be waiting
/// for the user to unlock 1Password).
pub const SETTLE_WAIT: Duration = Duration::from_secs(30);
/// How often a writer that died is started again (with every engine) before sync gives up
/// until the daemon restarts.
pub const MAX_WRITER_RESTARTS: u32 = 3;
/// How long a dead writer's task is given to report how it ended.
const DEAD_WRITER_GRACE: Duration = Duration::from_secs(1);

/// Clears the `checking` flag however the check ends, so a panic cannot stop the checks.
struct Checked<'a>(&'a AtomicBool);

impl Drop for Checked<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// What a provider factory gets to build one account's provider.
pub struct ProviderRequest {
    pub paths: Paths,
    pub config: Config,
    pub account: AccountConfig,
    pub ids: ItemIdCache,
    pub token_url: Option<String>,
    pub gmail_url: Option<String>,
    pub observer: RetryObserver,
}

/// Builds providers; runs on the blocking pool (it may run `op read`).
pub type ProviderFactory =
    Arc<dyn Fn(ProviderRequest) -> Result<Arc<dyn Provider>, OpError> + Send + Sync>;

/// An account the search operation asks.
pub struct SearchTarget {
    pub name: String,
    pub account_id: i64,
    pub provider: Arc<dyn Provider>,
}

/// What an engine was started with; a change restarts it.
#[derive(Clone, PartialEq)]
struct EngineKey {
    account: AccountConfig,
    settings: SyncSettings,
    secrets: SecretsBackend,
    onepassword: Option<OnePasswordConfig>,
}

fn key(config: &Config, account: &AccountConfig) -> EngineKey {
    EngineKey {
        account: account.clone(),
        settings: SyncSettings::for_account(&config.general, account),
        secrets: config.general.secrets,
        onepassword: config.onepassword.clone(),
    }
}

struct Running {
    key: EngineKey,
    name: String,
    account_id: i64,
    wake: Arc<Notify>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    provider: Arc<dyn Provider>,
}

#[derive(Default)]
struct Engines {
    running: HashMap<String, Running>,
    writer: Option<(WriterHandle, WriterTask)>,
    /// Accounts (by lowercase address) whose engine could not start, and why.
    failed: HashMap<String, OpError>,
    writer_restarts: u32,
    stopping: bool,
}

pub struct SyncManager {
    engines: tokio::sync::Mutex<Engines>,
    board: StatusBoard,
    factory: ProviderFactory,
    configured: AtomicUsize,
    config_mtime: Mutex<Option<SystemTime>>,
    /// True once the first reconcile has finished, well or not.
    settled: watch::Sender<bool>,
    /// Set once the writer died more often than `MAX_WRITER_RESTARTS`: sync is off.
    writer_failure: Mutex<Option<String>>,
    /// A periodic check is running; the next tick skips instead of queueing behind it.
    checking: AtomicBool,
}

fn not_syncing(who: &str) -> OpError {
    OpError::not_found(format!("no account named {who:?} is configured for sync"))
        .with_hint("`rail account list` shows the configured accounts")
}

fn unsupported() -> OpError {
    OpError::invalid_input("Microsoft 365 sync arrives in a later phase of Rail")
}

fn shutting_down() -> OpError {
    OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Transient,
        "the daemon is shutting down",
    )
}

fn named(status: &AccountSyncStatus, who: &str) -> bool {
    status.name.eq_ignore_ascii_case(who) || status.address.eq_ignore_ascii_case(who)
}

async fn stop_engine(running: Running) {
    let _ = running.stop.send(true);
    let mut task = running.task;
    if tokio::time::timeout(ENGINE_STOP, &mut task).await.is_err() {
        tracing::warn!(account = %running.name, "an engine did not stop in time; aborting it");
        task.abort();
    }
}

/// Drop the manager's handle and wait, at most `grace`, for the writer to commit what it has.
async fn finish_writer(writer: Option<(WriterHandle, WriterTask)>, grace: Duration) {
    if let Some((handle, task)) = writer {
        drop(handle);
        if !task.finish(grace).await {
            tracing::warn!("the store writer did not end cleanly");
        }
    }
}

fn retry_observer(board: StatusBoard, address: String) -> RetryObserver {
    Arc::new(move |wait: Duration| {
        let at = unix_now() + i64::try_from(wait.as_secs()).unwrap_or(0);
        board.update(&address, |s| {
            s.phase = SyncPhase::Retrying;
            s.next_attempt_at = Some(at);
        });
    })
}

/// The status an engine starts from: what the store remembers.
fn initial_status(account: &AccountConfig, state: &SyncStateRow) -> AccountSyncStatus {
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

/// The status of an account whose engine could not start.
fn failed_status(account: &AccountConfig, error: &OpError) -> AccountSyncStatus {
    let mut status = AccountSyncStatus::new(&account.name, &account.address, account.kind);
    status.phase = SyncPhase::Stopped;
    status.last_pass_failed = true;
    status.last_error = Some(SyncErrorInfo {
        code: "start".to_string(),
        message: error.message.clone(),
        at: unix_now(),
        detail: error.detail.clone(),
    });
    status
}

/// The config file's modification time and its contents; a missing file is no accounts.
fn read_config(path: &std::path::Path) -> (Option<SystemTime>, Result<Config, OpError>) {
    let mtime = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let config = match config::load(path) {
        Ok(config) => Ok(config),
        Err(ConfigError::Missing { .. }) => Ok(Config::default()),
        Err(error) => Err(error.into()),
    };
    (mtime, config)
}

impl SyncManager {
    pub fn new(factory: ProviderFactory) -> Self {
        Self {
            engines: tokio::sync::Mutex::new(Engines::default()),
            board: StatusBoard::default(),
            factory,
            configured: AtomicUsize::new(0),
            config_mtime: Mutex::new(None),
            settled: watch::Sender::new(false),
            writer_failure: Mutex::new(None),
            checking: AtomicBool::new(false),
        }
    }

    /// Wait (at most `SETTLE_WAIT`) for the first reconcile, so a request that arrives while
    /// the daemon is still starting its engines sees them.
    async fn settled(&self) {
        let mut settled = self.settled.subscribe();
        let _ = tokio::time::timeout(SETTLE_WAIT, settled.wait_for(|done| *done)).await;
    }

    pub fn board(&self) -> &StatusBoard {
        &self.board
    }

    /// True while `config.toml` lists any account: the daemon then never idles out.
    pub fn has_accounts(&self) -> bool {
        self.configured.load(Ordering::SeqCst) > 0
    }

    /// Count the configured accounts now, before the first reconcile finishes.
    pub fn count_accounts(&self, paths: &Paths) {
        let count = config::load(&paths.config_file()).map_or(0, |c| c.accounts.len());
        self.configured.store(count, Ordering::SeqCst);
    }

    /// Why sync is off for good, once the writer died more than `MAX_WRITER_RESTARTS` times.
    pub fn writer_failure(&self) -> Option<String> {
        self.writer_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Add sync's own trouble to a doctor report: a writer that is gone counts as a store error.
    pub fn add_to_report(&self, report: &mut DoctorReport) {
        if report.store_error.is_none()
            && let Some(failure) = self.writer_failure()
        {
            report.store_error = Some(Untrusted::new(failure));
        }
    }

    /// `reconcile` on a task of its own, so a slow `op read` never holds up the caller.
    pub fn reconcile_in_background(ctx: Arc<DaemonContext>) {
        tokio::spawn(async move { ctx.sync.reconcile(&ctx).await });
    }

    /// Start, stop and restart engines to match `config.toml`. Failures are logged and shown
    /// in the account's status; an invalid config leaves the running engines as they are.
    pub async fn reconcile(&self, ctx: &DaemonContext) {
        if let Err(error) = self.try_reconcile(ctx).await {
            tracing::warn!(error = %error.message, "sync could not follow the configuration");
        }
        self.settled.send_replace(true);
    }

    async fn try_reconcile(&self, ctx: &DaemonContext) -> Result<(), OpError> {
        let path = ctx.paths.config_file();
        let (mtime, config) = ctx.blocking(move || Ok(read_config(&path))).await?;
        // Remembered before the parse result counts: an invalid file is warned about once,
        // not again at every check.
        *self
            .config_mtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = mtime;
        let config = config?;
        self.configured
            .store(config.accounts.len(), Ordering::SeqCst);
        let mut engines = self.engines.lock().await;
        if engines.stopping {
            return Ok(());
        }
        self.stop_unwanted(&mut engines, &config).await;
        if self.writer_failure().is_some() {
            return Ok(());
        }
        for account in &config.accounts {
            self.ensure(ctx, &mut engines, &config, account).await;
        }
        Ok(())
    }

    async fn stop_unwanted(&self, engines: &mut Engines, config: &Config) {
        let keep: HashMap<String, EngineKey> = config
            .accounts
            .iter()
            .map(|a| (a.address.to_ascii_lowercase(), key(config, a)))
            .collect();
        let changed: Vec<String> = engines
            .running
            .iter()
            .filter(|(address, running)| keep.get(*address) != Some(&running.key))
            .map(|(address, _)| address.clone())
            .collect();
        for address in changed {
            if let Some(running) = engines.running.remove(&address) {
                stop_engine(running).await;
            }
        }
        engines
            .failed
            .retain(|address, _| keep.contains_key(address));
        for status in self.board.all() {
            if !keep.contains_key(&status.address) {
                self.board.remove(&status.address);
            }
        }
    }

    async fn ensure(
        &self,
        ctx: &DaemonContext,
        engines: &mut Engines,
        config: &Config,
        account: &AccountConfig,
    ) {
        let address = account.address.to_ascii_lowercase();
        if engines.running.contains_key(&address) {
            return;
        }
        if account.kind != AccountKind::Gmail {
            let mut status = AccountSyncStatus::new(&account.name, &address, account.kind);
            status.phase = SyncPhase::Unsupported;
            self.board.insert(status);
            return;
        }
        match self.start(ctx, engines, config, account).await {
            Ok(()) => {
                engines.failed.remove(&address);
            }
            Err(error) => {
                tracing::warn!(account = %account.name, error = %error.message, "could not start syncing an account");
                self.board.insert(failed_status(account, &error));
                engines.failed.insert(address, error);
            }
        }
    }

    async fn start(
        &self,
        ctx: &DaemonContext,
        engines: &mut Engines,
        config: &Config,
        account: &AccountConfig,
    ) -> Result<(), OpError> {
        let writer = Self::writer_in(ctx, engines)?;
        let (db, row) = (ctx.paths.db_file(), account.clone());
        let (account_id, state, reader) = ctx
            .blocking(move || {
                let store = Store::open(&db)?;
                let id = store.upsert_account(&row)?;
                Ok((id, store.sync_state(id)?, Store::open_reader(&db)?))
            })
            .await?;
        let provider = self.provider_for(ctx, config, account).await?;
        self.board.insert(initial_status(account, &state));
        let parts = EngineParts {
            account: account.clone(),
            account_id,
            settings: SyncSettings::for_account(&config.general, account),
            provider: provider.clone(),
            writer,
            reader,
            board: self.board.clone(),
            wake: Arc::new(Notify::new()),
            clock: Arc::new(unix_now),
        };
        let running = Self::spawn_engine(key(config, account), parts, provider);
        engines
            .running
            .insert(account.address.to_ascii_lowercase(), running);
        Ok(())
    }

    /// The account's provider, built on the blocking pool (an `op read` may wait on the user).
    async fn provider_for(
        &self,
        ctx: &DaemonContext,
        config: &Config,
        account: &AccountConfig,
    ) -> Result<Arc<dyn Provider>, OpError> {
        let request = ProviderRequest {
            paths: ctx.paths.clone(),
            config: config.clone(),
            account: account.clone(),
            ids: ctx.op_ids.clone(),
            token_url: ctx.token_url().map(str::to_string),
            gmail_url: ctx.gmail_url().map(str::to_string),
            observer: retry_observer(self.board.clone(), account.address.to_ascii_lowercase()),
        };
        let factory = self.factory.clone();
        ctx.blocking(move || factory(request)).await
    }

    fn spawn_engine(key: EngineKey, parts: EngineParts, provider: Arc<dyn Provider>) -> Running {
        let (name, account_id) = (parts.account.name.clone(), parts.account_id);
        let engine = Engine::new(parts);
        let wake = engine.wake_handle();
        let (stop, stopped) = watch::channel(false);
        let task = tokio::spawn(engine.run(stopped));
        Running {
            key,
            name,
            account_id,
            wake,
            stop,
            task,
            provider,
        }
    }

    fn writer_in(ctx: &DaemonContext, engines: &mut Engines) -> Result<WriterHandle, OpError> {
        if let Some((writer, _)) = &engines.writer {
            return Ok(writer.clone());
        }
        let (writer, task) = spawn_writer(&ctx.paths.db_file())?;
        engines.writer = Some((writer.clone(), task));
        Ok(writer)
    }

    /// The writer, started if needed (search writes through it too). Hold the handle only for
    /// a write: shutdown waits for every handle to go before the writer can end.
    pub async fn writer(&self, ctx: &DaemonContext) -> Result<WriterHandle, OpError> {
        let mut engines = self.engines.lock().await;
        if engines.stopping {
            return Err(shutting_down());
        }
        if let Some(failure) = self.writer_failure() {
            return Err(OpError::new(
                ErrorCode::Store,
                ErrorClass::Permanent,
                failure,
            ));
        }
        Self::writer_in(ctx, &mut engines)
    }

    /// Every `CONFIG_CHECK`: look at the writer, then at `config.toml`, on a task of its own.
    /// A tick that finds the previous check still running skips.
    pub fn tick(ctx: &Arc<DaemonContext>) {
        if ctx.sync.checking.swap(true, Ordering::SeqCst) {
            return;
        }
        let ctx = ctx.clone();
        tokio::spawn(async move {
            let _done = Checked(&ctx.sync.checking);
            ctx.sync.check_writer(&ctx).await;
            ctx.sync.refresh_if_changed(&ctx).await;
        });
    }

    /// Reconcile when `config.toml` changed on disk since the last look.
    pub async fn refresh_if_changed(&self, ctx: &DaemonContext) {
        let path = ctx.paths.config_file();
        let now = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        let seen = *self
            .config_mtime
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if now != seen {
            self.reconcile(ctx).await;
        }
    }

    /// Notice a writer that died (it panicked, or its thread ended): every engine's writes
    /// would fail from now on. Restart it with every engine, at most `MAX_WRITER_RESTARTS`
    /// times; after that stop syncing and say so in each account's status and in doctor.
    pub async fn check_writer(&self, ctx: &DaemonContext) {
        let mut engines = self.engines.lock().await;
        let dead = engines.writer.as_ref().is_some_and(|(w, _)| w.is_closed());
        if !dead || engines.stopping {
            return;
        }
        for (_, running) in engines.running.drain() {
            stop_engine(running).await;
        }
        finish_writer(engines.writer.take(), DEAD_WRITER_GRACE).await;
        if engines.writer_restarts >= MAX_WRITER_RESTARTS {
            self.give_up_on_the_writer();
            return;
        }
        engines.writer_restarts += 1;
        tracing::warn!(
            attempt = engines.writer_restarts,
            "the store writer stopped; starting it and every engine again"
        );
        drop(engines);
        self.reconcile(ctx).await;
    }

    fn give_up_on_the_writer(&self) {
        let message = format!(
            "the store writer stopped {} times; sync is paused until the daemon restarts (`rail daemon stop`)",
            MAX_WRITER_RESTARTS + 1
        );
        tracing::error!("{message}");
        let at = unix_now();
        for status in self.board.all() {
            self.board.update(&status.address, |s| {
                s.phase = SyncPhase::Stopped;
                s.pass_running = false;
                s.last_pass_failed = true;
                s.next_attempt_at = None;
                s.last_error = Some(SyncErrorInfo {
                    code: "store".to_string(),
                    message: message.clone(),
                    at,
                    detail: None,
                });
            });
        }
        *self
            .writer_failure
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(message);
    }

    /// After a successful login: clear `needs_auth` and wake the account (or start it).
    pub async fn logged_in(&self, ctx: &DaemonContext, address: &str) {
        let key = address.to_ascii_lowercase();
        let running = {
            let engines = self.engines.lock().await;
            engines
                .running
                .get(&key)
                .map(|r| (r.account_id, r.wake.clone()))
        };
        let Some((account_id, wake)) = running else {
            return self.reconcile(ctx).await;
        };
        self.board.update(&key, |s| {
            s.needs_auth = false;
            if s.phase == SyncPhase::NeedsAuth {
                s.phase = SyncPhase::Idle;
            }
        });
        let clear = Batch {
            state: StateUpdate {
                needs_auth: Some(false),
                ..StateUpdate::default()
            },
            ..Batch::new(account_id)
        };
        let written = match self.writer(ctx).await {
            Ok(writer) => writer.write(clear).await.map(|_| ()),
            Err(error) => Err(error),
        };
        if let Err(error) = written {
            tracing::warn!(error = %error.message, "could not clear needs_auth");
        }
        wake.notify_one();
    }

    /// Wake the named account's engine (or every engine) and say which pass to wait for.
    /// `notify_one` keeps a permit, so a wake that arrives during a pass is not lost.
    pub async fn wake(&self, who: Option<&str>) -> Result<Vec<SyncTarget>, OpError> {
        self.settled().await;
        let engines = self.engines.lock().await;
        let mut targets = Vec::new();
        for (address, running) in &engines.running {
            let Some(status) = self.board.get(address) else {
                continue;
            };
            if who.is_some_and(|who| !named(&status, who)) {
                continue;
            }
            let pass = status.passes_completed + if status.pass_running { 2 } else { 1 };
            running.wake.notify_one();
            targets.push(SyncTarget {
                name: status.name,
                address: address.clone(),
                pass,
            });
        }
        match who {
            Some(who) if targets.is_empty() => Err(self.why_not(&engines, who)),
            _ => Ok(targets),
        }
    }

    /// Why `who` has no engine: Microsoft 365, an engine that failed to start (its error), a
    /// writer that is gone, or no such account.
    fn why_not(&self, engines: &Engines, who: &str) -> OpError {
        let all = self.board.all();
        let Some(status) = all.iter().find(|s| named(s, who)) else {
            return not_syncing(who);
        };
        if status.phase == SyncPhase::Unsupported {
            return unsupported();
        }
        if let Some(error) = engines.failed.get(&status.address) {
            return error.clone();
        }
        match self.writer_failure() {
            Some(failure) => OpError::new(ErrorCode::Store, ErrorClass::Permanent, failure),
            None => not_syncing(who),
        }
    }

    pub async fn status(&self, who: Option<&str>) -> Result<Vec<AccountSyncStatus>, OpError> {
        self.settled().await;
        let all = self.board.all();
        let Some(who) = who else {
            return Ok(all);
        };
        let found: Vec<_> = all.into_iter().filter(|s| named(s, who)).collect();
        if found.is_empty() {
            Err(not_syncing(who))
        } else {
            Ok(found)
        }
    }

    /// `subscriber` runs under the board's lock: it must not block or call the board.
    pub fn subscribe(&self, subscriber: Subscriber) {
        self.board.subscribe(subscriber);
    }

    /// The running engines' providers, all or the one for `address`.
    pub async fn search_targets(&self, address: Option<&str>) -> Vec<SearchTarget> {
        self.settled().await;
        let engines = self.engines.lock().await;
        engines
            .running
            .iter()
            .filter(|(a, _)| address.is_none_or(|want| a.eq_ignore_ascii_case(want)))
            .map(|(_, r)| SearchTarget {
                name: r.name.clone(),
                account_id: r.account_id,
                provider: r.provider.clone(),
            })
            .collect()
    }

    /// Stop every engine at its next await point, then let the writer commit the batch in
    /// hand, waiting at most `grace` for it.
    pub async fn shutdown(&self, grace: Duration) {
        let Ok(mut engines) = tokio::time::timeout(grace, self.engines.lock()).await else {
            tracing::warn!("sync was still starting an account at shutdown");
            return;
        };
        engines.stopping = true;
        for (_, running) in engines.running.drain() {
            stop_engine(running).await;
        }
        finish_writer(engines.writer.take(), grace).await;
    }
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
