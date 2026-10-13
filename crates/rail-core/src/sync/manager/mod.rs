//! The daemon's sync: one engine per configured account, the writer they share and the status
//! board, kept in step with `config.toml`.
//!
//! Reconciles run one at a time. Each holds the engines lock only to decide what to start and
//! to insert what it built: the slow part (the store rows and the provider, which may wait on
//! `op read`) runs without it, so `sync.now`, `search`, the writer checks and shutdown never
//! wait behind a locked 1Password.

mod outbox;
mod parts;
mod supervise;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::watch;
use tokio::time::Instant;

pub use self::outbox::{ActionTarget, Wants};
use self::parts::{
    ConfigStamp, Engines, PlannedAccount, Prepared, Running, failed_status, finish_writer,
    initial_status, key, named, needs_login, not_running, not_syncing, read_config, retry_observer,
    shutting_down, stop_engine, stop_engine_within, unsupported, writer_gone,
};
pub(crate) use self::parts::{retry_delay, unobserved};
use super::batch::{Batch, StateUpdate};
use super::engine::{Engine, EngineParts};
use super::outbox::{Worker, WorkerParts};
use super::status::{StatusBoard, Subscriber};
use super::writer::{WriterHandle, spawn_writer};
use crate::auth::{ItemIdCache, unix_now};
use crate::config::{self, AccountConfig, AccountKind, Config, SyncSettings};
use crate::daemon::DaemonContext;
use crate::ops::{
    AccountSyncStatus, DoctorReport, OpError, SearchFailure, SyncPhase, SyncTarget, Untrusted,
};
use crate::paths::Paths;
use crate::provider::Provider;
use crate::provider::http::RetryObserver;
use crate::store::Store;

/// How often the daemon looks at `config.toml`'s modification time (and at the writer and
/// the engines).
pub const CONFIG_CHECK: Duration = Duration::from_secs(2);
/// How long a stopping engine may take before its task is aborted.
pub const ENGINE_STOP: Duration = Duration::from_secs(5);
/// The longest a request waits for the daemon's first reconcile (an `op read` may be waiting
/// for the user to unlock 1Password).
pub const SETTLE_WAIT: Duration = Duration::from_secs(30);
/// How often a writer that died is started again (with every engine) before sync gives up
/// until the daemon restarts.
pub const MAX_WRITER_RESTARTS: u32 = 3;
/// A writer that ran this long without dying starts its restart count again.
pub const WRITER_HEALTHY: Duration = Duration::from_secs(3600);

/// What a provider factory gets to build one account's provider.
pub struct ProviderRequest {
    pub paths: Paths,
    pub config: Config,
    pub account: AccountConfig,
    pub ids: ItemIdCache,
    pub token_url: Option<String>,
    pub gmail_url: Option<String>,
    /// The Graph API root used instead of Microsoft's (a debug-build test hook).
    pub graph_url: Option<String>,
    pub observer: RetryObserver,
}

/// Builds providers; runs on the blocking pool (it may run `op read`).
pub type ProviderFactory =
    Arc<dyn Fn(ProviderRequest) -> Result<Arc<dyn Provider>, OpError> + Send + Sync>;

/// An account the search operation asks.
pub struct SearchTarget {
    pub name: String,
    /// The account's kind: its provider's name in a failure.
    pub kind: AccountKind,
    pub account_id: i64,
    pub provider: Arc<dyn Provider>,
}

/// Whether an account can fetch bodies now: its engine's provider, or the error every id
/// gets.
pub enum Reach {
    Ready(SearchTarget),
    Unavailable(OpError),
}

pub struct SyncManager {
    engines: tokio::sync::Mutex<Engines>,
    /// Held for a whole reconcile, so two never interleave; nothing else takes it.
    reconciling: tokio::sync::Mutex<()>,
    board: StatusBoard,
    factory: ProviderFactory,
    configured: AtomicUsize,
    /// `config.toml` as the last reconcile read it, for the periodic change check.
    config_stamp: Mutex<ConfigStamp>,
    /// The last `config.toml` that parsed, for a writer restart while the file is invalid.
    last_config: Mutex<Option<Config>>,
    /// True once the first reconcile has finished, well or not.
    settled: watch::Sender<bool>,
    /// Set once the writer died more often than `MAX_WRITER_RESTARTS`: sync is off.
    writer_failure: Mutex<Option<String>>,
    /// A periodic check is running; the next tick skips instead of queueing behind it.
    checking: AtomicBool,
    /// Set first thing at shutdown: nothing starts after it.
    stopping: AtomicBool,
    /// Reconciles that failed as a whole (each logged a warning).
    #[cfg(test)]
    failed_reconciles: AtomicUsize,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl SyncManager {
    pub fn new(factory: ProviderFactory) -> Self {
        Self {
            engines: tokio::sync::Mutex::new(Engines::default()),
            reconciling: tokio::sync::Mutex::new(()),
            board: StatusBoard::default(),
            factory,
            configured: AtomicUsize::new(0),
            config_stamp: Mutex::new(ConfigStamp::default()),
            last_config: Mutex::new(None),
            settled: watch::Sender::new(false),
            writer_failure: Mutex::new(None),
            checking: AtomicBool::new(false),
            stopping: AtomicBool::new(false),
            #[cfg(test)]
            failed_reconciles: AtomicUsize::new(0),
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

    fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
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
        lock(&self.writer_failure).clone()
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
    /// in the account's status; an invalid config leaves the running engines as they are. An
    /// account whose start failed waits out its backoff unless its settings changed, so
    /// reconciles that follow one another never run `op read` for it twice.
    pub async fn reconcile(&self, ctx: &DaemonContext) {
        self.reconcile_with(ctx, false, None).await;
    }

    /// `reconcile`, trying the account `who` names again now even if its backoff has not
    /// passed (it was asked for by name, or just logged in).
    pub async fn reconcile_now(&self, ctx: &DaemonContext, who: &str) {
        self.reconcile_with(ctx, false, Some(who)).await;
    }

    /// `reconcile`; with `fallback`, an invalid `config.toml` means the last one that parsed;
    /// `now` names an account tried again whatever its backoff.
    async fn reconcile_with(&self, ctx: &DaemonContext, fallback: bool, now: Option<&str>) {
        if let Err(error) = self.try_reconcile(ctx, fallback, now).await {
            tracing::warn!(error = %error.message, "sync could not follow the configuration");
            #[cfg(test)]
            self.failed_reconciles.fetch_add(1, Ordering::SeqCst);
            // Not again at the next tick: the failed accounts wait their backoff once more.
            self.engines.lock().await.postpone_retries();
        }
        self.settled.send_replace(true);
    }

    async fn try_reconcile(
        &self,
        ctx: &DaemonContext,
        fallback: bool,
        now: Option<&str>,
    ) -> Result<(), OpError> {
        let _one_at_a_time = self.reconciling.lock().await;
        let config = self.load_config(ctx, fallback).await?;
        if self.is_stopping() {
            return Ok(());
        }
        for planned in self.plan(ctx, &config, now).await? {
            // No further `op read` once shutdown began.
            if self.is_stopping() {
                break;
            }
            let prepared = self.prepare(ctx, &config, &planned.account).await;
            self.install(ctx, &planned, prepared).await;
        }
        Ok(())
    }

    async fn load_config(&self, ctx: &DaemonContext, fallback: bool) -> Result<Config, OpError> {
        let path = ctx.paths.config_file();
        let (stamp, parsed) = ctx.blocking(move || Ok(read_config(&path))).await?;
        // Remembered before the parse result counts: an invalid file is warned about once,
        // not again at every check.
        *lock(&self.config_stamp) = stamp;
        let config = match parsed {
            Ok(config) => {
                *lock(&self.last_config) = Some(config.clone());
                config
            }
            Err(error) => match lock(&self.last_config).clone().filter(|_| fallback) {
                Some(last) => {
                    tracing::warn!(error = %error.message, "config.toml is invalid; sync uses the last one that loaded");
                    last
                }
                None => return Err(error),
            },
        };
        self.configured
            .store(config.accounts.len(), Ordering::SeqCst);
        Ok(config)
    }

    /// Under the engines lock: stop what is no longer wanted and say which accounts need an
    /// engine (not a failed one still waiting out its backoff, unless `now` names it). Opens
    /// the writer first, so the store exists before anything reads it.
    async fn plan(
        &self,
        ctx: &DaemonContext,
        config: &Config,
        now: Option<&str>,
    ) -> Result<Vec<PlannedAccount>, OpError> {
        let mut engines = self.engines.lock().await;
        if self.is_stopping() {
            return Ok(Vec::new());
        }
        self.stop_unwanted(&mut engines, config).await;
        if self.writer_failure().is_some() {
            engines.postpone_retries();
            return Ok(Vec::new());
        }
        let mut wanted = Vec::new();
        for account in &config.accounts {
            let address = account.address.to_ascii_lowercase();
            if engines.running.contains_key(&address) {
                continue;
            }
            if super::providers::syncs(account.kind) {
                let key = key(config, account);
                let asked = now.is_some_and(|who| {
                    account.name.eq_ignore_ascii_case(who)
                        || account.address.eq_ignore_ascii_case(who)
                });
                if !asked && engines.waiting(&address, &key) {
                    continue;
                }
                wanted.push(PlannedAccount {
                    account: account.clone(),
                    key,
                    settings: SyncSettings::for_account(&config.general, account),
                });
            } else {
                let mut status = AccountSyncStatus::new(&account.name, &address, account.kind);
                status.phase = SyncPhase::Unsupported;
                self.board.insert(status);
            }
        }
        if !wanted.is_empty() {
            drop(Self::writer_in(ctx, &mut engines)?);
        }
        Ok(wanted)
    }

    async fn stop_unwanted(&self, engines: &mut Engines, config: &Config) {
        let keep: std::collections::HashMap<String, parts::EngineKey> = config
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
        engines
            .attempts
            .retain(|address, _| keep.contains_key(address));
        for status in self.board.all() {
            if !keep.contains_key(&status.address) {
                self.board.remove(&status.address);
            }
        }
    }

    /// Without the engines lock: the account's store rows, a reader and its provider.
    async fn prepare(
        &self,
        ctx: &DaemonContext,
        config: &Config,
        account: &AccountConfig,
    ) -> Result<Prepared, OpError> {
        let (db, row) = (ctx.paths.db_file(), account.clone());
        let (account_id, state, reader, outbox_reader) = ctx
            .blocking(move || {
                let store = Store::open(&db)?;
                let id = store.upsert_account(&row)?;
                Ok((
                    id,
                    store.sync_state(id)?,
                    Store::open_reader(&db)?,
                    Store::open_reader(&db)?,
                ))
            })
            .await?;
        let provider = self.provider_for(ctx, config, account).await?;
        Ok(Prepared {
            account_id,
            state,
            reader,
            outbox_reader,
            provider,
        })
    }

    /// Under the engines lock again: start the engine, or record why it could not start.
    /// What was built is dropped when, meanwhile, the daemon began stopping, the writer was
    /// given up, the account got an engine, or `config.toml` no longer has the account as it
    /// was planned (the next check reconciles the new file).
    async fn install(
        &self,
        ctx: &DaemonContext,
        planned: &PlannedAccount,
        prepared: Result<Prepared, OpError>,
    ) {
        let still = self.still_wanted(ctx, planned).await;
        let mut engines = self.engines.lock().await;
        let address = planned.account.address.to_ascii_lowercase();
        if !still
            || self.is_stopping()
            || self.writer_failure().is_some()
            || engines.running.contains_key(&address)
        {
            return;
        }
        let account = &planned.account;
        let started = prepared.and_then(|prepared| {
            let writer = Self::writer_in(ctx, &mut engines)?;
            Ok(self.spawn_engine(planned, prepared, writer, ctx.paths.draft_files_dir()))
        });
        match started {
            Ok(running) => {
                engines.failed.remove(&address);
                engines.running.insert(address, running);
            }
            Err(error) => {
                tracing::warn!(account = %account.name, error = %error.message, "could not start syncing an account");
                self.board.insert(failed_status(account, &error));
                engines.record_failure(&address, planned.key.clone(), error);
            }
        }
    }

    /// True when `config.toml` still has the account with the settings it was planned with.
    /// A file that does not load now counts as unchanged (an invalid edit leaves sync alone).
    async fn still_wanted(&self, ctx: &DaemonContext, planned: &PlannedAccount) -> bool {
        let path = ctx.paths.config_file();
        let Ok((_, Ok(config))) = ctx.blocking(move || Ok(read_config(&path))).await else {
            return true;
        };
        let address = &planned.account.address;
        config
            .accounts
            .iter()
            .find(|a| a.address.eq_ignore_ascii_case(address))
            .is_some_and(|a| key(&config, a) == planned.key)
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
            graph_url: ctx.graph_url().map(str::to_string),
            observer: retry_observer(self.board.clone(), account.address.to_ascii_lowercase()),
        };
        let factory = self.factory.clone();
        ctx.blocking(move || factory(request)).await
    }

    fn new_engine(
        &self,
        planned: &PlannedAccount,
        (account_id, provider): (i64, Arc<dyn Provider>),
        reader: Store,
        writer: &WriterHandle,
    ) -> Engine {
        Engine::new(EngineParts {
            account: planned.account.clone(),
            account_id,
            settings: planned.settings,
            provider,
            writer: writer.clone(),
            reader,
            board: self.board.clone(),
            wake: Arc::new(tokio::sync::Notify::new()),
            clock: Arc::new(unix_now),
        })
    }

    fn spawn_engine(
        &self,
        planned: &PlannedAccount,
        prepared: Prepared,
        writer: WriterHandle,
        draft_files_dir: PathBuf,
    ) -> Running {
        let account = &planned.account;
        self.board.insert(initial_status(account, &prepared.state));
        let engine = self.new_engine(
            planned,
            (prepared.account_id, prepared.provider.clone()),
            prepared.reader,
            &writer,
        );
        let wake = engine.wake_handle();
        let outbox_signal = Arc::new(tokio::sync::Notify::new());
        let worker = Worker::new(WorkerParts {
            name: account.name.clone(),
            address: account.address.to_ascii_lowercase(),
            account_id: prepared.account_id,
            provider: prepared.provider.clone(),
            writer,
            reader: prepared.outbox_reader,
            board: self.board.clone(),
            signal: outbox_signal.clone(),
            clock: Arc::new(unix_now),
            from_name: account.from_name.clone(),
            sync_wake: wake.clone(),
            draft_files_dir,
            kind: account.kind,
        });
        let (stop, stopped) = watch::channel(false);
        let outbox_task = tokio::spawn(worker.run(stopped.clone()));
        Running {
            key: planned.key.clone(),
            name: account.name.clone(),
            account_id: prepared.account_id,
            wake,
            stop,
            task: tokio::spawn(engine.run(stopped)),
            outbox_signal,
            outbox_task,
            provider: prepared.provider,
        }
    }

    fn writer_in(ctx: &DaemonContext, engines: &mut Engines) -> Result<WriterHandle, OpError> {
        if let Some((writer, _)) = &engines.writer {
            return Ok(writer.clone());
        }
        let (writer, task) = spawn_writer(&ctx.paths.db_file())?;
        engines.writer = Some((writer.clone(), task));
        engines.writer_started = Some(Instant::now());
        Ok(writer)
    }

    /// The writer, started if needed (search writes through it too). Hold the handle only for
    /// a write: shutdown waits for every handle to go before the writer can end.
    pub async fn writer(&self, ctx: &DaemonContext) -> Result<WriterHandle, OpError> {
        if self.is_stopping() {
            return Err(shutting_down());
        }
        let mut engines = self.engines.lock().await;
        if self.is_stopping() {
            return Err(shutting_down());
        }
        if let Some(failure) = self.writer_failure() {
            return Err(writer_gone(failure));
        }
        Self::writer_in(ctx, &mut engines)
    }

    /// After a successful login: clear `needs_auth` and wake the account (or start it).
    pub async fn logged_in(&self, ctx: &DaemonContext, address: &str) {
        let key = address.to_ascii_lowercase();
        let running = {
            let engines = self.engines.lock().await;
            engines
                .running
                .get(&key)
                .map(|r| (r.account_id, r.wake.clone(), r.outbox_signal.clone()))
        };
        let Some((account_id, wake, outbox_signal)) = running else {
            return self.reconcile_now(ctx, &key).await;
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
        outbox_signal.notify_one();
    }

    /// Wake the named account's engine (or every engine) and say which pass to wait for.
    /// `notify_one` keeps a permit, so a wake that arrives during a pass is not lost. An
    /// account named here whose engine failed is tried again first, inline (the caller asked
    /// for that account); a wake of every account retries, in the background, only the failed
    /// accounts whose backoff has passed, so running engines are woken at once even while a
    /// build waits on 1Password, and repeated wakes never prompt for 1Password again and again.
    pub async fn wake(
        &self,
        ctx: &Arc<DaemonContext>,
        who: Option<&str>,
    ) -> Result<Vec<SyncTarget>, OpError> {
        self.settled().await;
        match who {
            Some(who) if self.has_failed(Some(who)).await => self.reconcile_now(ctx, who).await,
            Some(_) => {}
            None if self.engines.lock().await.retry_due()
                && self.reconciling.try_lock().is_ok() =>
            {
                // No reconcile in flight: start one for the failed accounts that are due.
                Self::reconcile_in_background(ctx.clone());
            }
            None => {}
        }
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

    /// True when an account `who` names (any, when `None`) has no engine because it failed.
    async fn has_failed(&self, who: Option<&str>) -> bool {
        let engines = self.engines.lock().await;
        engines.failed.keys().any(|address| {
            who.is_none_or(|who| self.board.get(address).is_some_and(|s| named(&s, who)))
        })
    }

    /// Why `who` has no engine: a kind Rail does not sync, an engine that failed (its error), a writer
    /// that is gone, or no such account.
    fn why_not(&self, engines: &Engines, who: &str) -> OpError {
        let all = self.board.all();
        let Some(status) = all.iter().find(|s| named(s, who)) else {
            return not_syncing(who);
        };
        self.reason(engines, status)
    }

    /// Why the account behind `status` has no running engine.
    fn reason(&self, engines: &Engines, status: &AccountSyncStatus) -> OpError {
        if status.phase == SyncPhase::Unsupported {
            return unsupported();
        }
        if let Some(failed) = engines.failed.get(&status.address) {
            return failed.error.clone();
        }
        match self.writer_failure() {
            Some(failure) => writer_gone(failure),
            None => not_running(),
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

    /// The running engines' providers (all, or the one for `address`), and for every other
    /// configured account why the server is not asked.
    pub async fn search_plan(
        &self,
        address: Option<&str>,
    ) -> (Vec<SearchTarget>, Vec<SearchFailure>) {
        self.settled().await;
        let engines = self.engines.lock().await;
        let mut targets = Vec::new();
        let mut skipped = Vec::new();
        let wanted = |a: &str| address.is_none_or(|want| a.eq_ignore_ascii_case(want));
        for status in self.board.all().iter().filter(|s| wanted(&s.address)) {
            match engines.running.get(&status.address) {
                Some(r) => targets.push(SearchTarget {
                    name: r.name.clone(),
                    kind: status.kind,
                    account_id: r.account_id,
                    provider: r.provider.clone(),
                }),
                None => skipped.push(SearchFailure {
                    account: status.name.clone(),
                    kind: status.kind,
                    error: self.reason(&engines, status),
                }),
            }
        }
        (targets, skipped)
    }

    /// The account `who` names (name or address), for `body.fetch`. An account no one
    /// configured is an error; one paused for a login, of a kind Rail does not sync, or one without a running
    /// engine is `Unavailable` with the reason.
    pub async fn body_target(&self, who: &str) -> Result<Reach, OpError> {
        self.settled().await;
        let engines = self.engines.lock().await;
        let all = self.board.all();
        let Some(status) = all.iter().find(|s| named(s, who)) else {
            return Err(not_syncing(who));
        };
        if status.needs_auth {
            return Ok(Reach::Unavailable(needs_login(&status.name)));
        }
        Ok(match engines.running.get(&status.address) {
            Some(running) => Reach::Ready(SearchTarget {
                name: running.name.clone(),
                kind: status.kind,
                account_id: running.account_id,
                provider: running.provider.clone(),
            }),
            None => Reach::Unavailable(self.reason(&engines, status)),
        })
    }

    /// Refuse new starts at once, then stop every engine at its next await point (all at
    /// once, each aborted after `ENGINE_STOP`) and let the writer commit the batch in hand.
    /// The whole of it (the wait for the engines lock, the engines, the writer) takes at most
    /// `grace`, one deadline for every step, so the daemon's own exit budget holds.
    pub async fn shutdown(&self, grace: Duration) {
        self.stopping.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + grace;
        let left = || deadline.saturating_duration_since(Instant::now());
        let Ok(mut engines) = tokio::time::timeout_at(deadline, self.engines.lock()).await else {
            tracing::warn!("the sync engines were still busy at shutdown");
            return;
        };
        let stopping: Vec<Running> = engines.running.drain().map(|(_, r)| r).collect();
        futures_util::future::join_all(
            stopping
                .into_iter()
                .map(|running| stop_engine_within(running, left())),
        )
        .await;
        finish_writer(engines.writer.take(), left()).await;
    }
}

#[cfg(test)]
mod outbox_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod window_tests;
