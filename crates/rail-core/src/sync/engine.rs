//! One account's sync: passes until the daemon stops it. The pass steps are in
//! `pass.rs`. After a pass the engine sleeps until the poll interval passes, it is woken, or
//! it is stopped; `NeedsAuth` pauses it until it is woken. Stopping drops the pass at its next
//! await point; a batch already handed to the writer still commits.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, watch};

use super::batch::{Batch, BatchOutcome, StateUpdate};
use super::status::StatusBoard;
use super::writer::WriterHandle;
use crate::config::{AccountConfig, SyncSettings};
use crate::error::ErrorClass;
use crate::ops::{AccountSyncStatus, OpError, SyncErrorInfo, SyncPhase, Untrusted};
use crate::provider::{Provider, ProviderError};
use crate::store::{Store, StoreError};

/// Unix seconds; tests pass a fixed one.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

#[derive(Debug)]
pub enum SyncError {
    Provider(ProviderError),
    Store(OpError),
}

impl From<ProviderError> for SyncError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error)
    }
}

impl From<OpError> for SyncError {
    fn from(error: OpError) -> Self {
        Self::Store(error)
    }
}

impl From<StoreError> for SyncError {
    fn from(error: StoreError) -> Self {
        Self::Store(error.into())
    }
}

impl SyncError {
    pub fn code(&self) -> String {
        match self {
            Self::Provider(e) => e.code().to_string(),
            Self::Store(_) => "store".to_string(),
        }
    }

    /// Rail's own words.
    pub fn message(&self) -> String {
        match self {
            Self::Provider(e) => e.to_string(),
            Self::Store(e) => e.message.clone(),
        }
    }

    /// The provider's or the system's words.
    pub fn detail(&self) -> Option<String> {
        match self {
            Self::Provider(e) => e.detail().map(str::to_string),
            Self::Store(e) => e.detail.as_ref().map(|d| d.get().clone()),
        }
    }

    /// Whether retrying can help.
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Provider(e) => e.class(),
            Self::Store(e) => e.class,
        }
    }

    pub fn needs_auth(&self) -> bool {
        matches!(self, Self::Provider(ProviderError::NeedsAuth { .. }))
    }
}

pub struct EngineParts {
    pub account: AccountConfig,
    pub account_id: i64,
    pub settings: SyncSettings,
    pub provider: Arc<dyn Provider>,
    pub writer: WriterHandle,
    /// A `Store::open_reader` connection of its own.
    pub reader: Store,
    pub board: StatusBoard,
    pub wake: Arc<Notify>,
    /// The account's outbox worker: a batch that makes snoozes due wakes it.
    pub outbox: Arc<Notify>,
    pub clock: Clock,
}

pub struct Engine {
    pub(super) account: AccountConfig,
    pub(super) account_id: i64,
    pub(super) settings: SyncSettings,
    pub(super) provider: Arc<dyn Provider>,
    writer: WriterHandle,
    reader: Arc<Mutex<Store>>,
    board: StatusBoard,
    wake: Arc<Notify>,
    outbox: Arc<Notify>,
    pub(super) clock: Clock,
}

pub(crate) async fn stopped(stop: &mut watch::Receiver<bool>) {
    // A dropped sender also means stop.
    let _ = stop.wait_for(|stop| *stop).await;
}

async fn sleep(poll: Option<Duration>) {
    match poll {
        Some(wait) => tokio::time::sleep(wait).await,
        None => std::future::pending().await,
    }
}

impl Engine {
    pub fn new(parts: EngineParts) -> Self {
        Self {
            account: parts.account,
            account_id: parts.account_id,
            settings: parts.settings,
            provider: parts.provider,
            writer: parts.writer,
            reader: Arc::new(Mutex::new(parts.reader)),
            board: parts.board,
            wake: parts.wake,
            outbox: parts.outbox,
            clock: parts.clock,
        }
    }

    /// The notifier that wakes this engine (`sync.now`, a login).
    pub fn wake_handle(&self) -> Arc<Notify> {
        self.wake.clone()
    }

    /// Run passes until `stop` turns true or its sender goes.
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        loop {
            let outcome = tokio::select! {
                outcome = self.pass() => outcome,
                () = stopped(&mut stop) => break,
            };
            let paused = self.record(outcome).await;
            let poll = (!paused).then_some(self.settings.poll_interval);
            tokio::select! {
                () = sleep(poll) => {}
                () = self.wake.notified() => {}
                () = stopped(&mut stop) => break,
            }
        }
        self.status(|s| {
            s.phase = SyncPhase::Stopped;
            s.pass_running = false;
        });
    }

    /// Change this account's status (and publish it).
    pub(super) fn status(&self, change: impl FnOnce(&mut AccountSyncStatus)) {
        self.board.update(&self.account.address, change);
    }

    pub(super) fn phase(&self, phase: SyncPhase) {
        self.status(|s| s.phase = phase);
    }

    /// Await one provider call. A retry inside it shows as `Retrying` (the HTTP layer's retry
    /// observer sets that); once the call returns, the step's own `phase` shows again.
    pub(super) async fn call<T>(
        &self,
        phase: SyncPhase,
        call: impl Future<Output = Result<T, ProviderError>>,
    ) -> Result<T, ProviderError> {
        let result = call.await;
        let retrying = self
            .board
            .get(&self.account.address)
            .is_some_and(|s| s.phase == SyncPhase::Retrying);
        if retrying {
            self.status(|s| {
                s.phase = phase;
                s.next_attempt_at = None;
            });
        }
        result
    }

    /// Run `read` on this engine's reader connection, on the blocking pool.
    pub(super) async fn read<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, SyncError> {
        let reader = self.reader.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let store = reader.lock().unwrap_or_else(PoisonError::into_inner);
            read(&store)
        })
        .await;
        match joined {
            Ok(result) => Ok(result?),
            Err(_) => Err(OpError::internal("a store read failed to run").into()),
        }
    }

    /// Hand `batch` to the writer, stamped with this engine's clock, and wait for its commit.
    /// A batch that settled failed actions publishes the account's outbox depth; one whose mail
    /// made snoozes due wakes the outbox worker, which puts them back.
    pub(super) async fn write(&self, mut batch: Batch) -> Result<BatchOutcome, SyncError> {
        batch.account_id = self.account_id;
        batch.at = Some((self.clock)());
        let outcome = self.writer.write(batch).await?;
        if outcome.snoozes_due > 0 {
            self.outbox.notify_one();
        }
        if outcome.failed_cleared > 0 {
            let (board, address, id) = (
                self.board.clone(),
                self.account.address.clone(),
                self.account_id,
            );
            let published = self
                .read(move |store| {
                    super::outbox::publish_depth(&board, &address, store, id);
                    Ok(())
                })
                .await;
            if let Err(error) = published {
                tracing::warn!(account = %self.account.name, code = %error.code(), "could not publish the outbox depth");
            }
        }
        Ok(outcome)
    }

    /// Record how a pass ended in the store and the status; a provider's standing error makes a
    /// pass that went through a failed one. True when the engine must pause until it is woken (the
    /// account needs a login).
    pub(crate) async fn record(&self, outcome: Result<(), SyncError>) -> bool {
        // A pass that went through still fails with what the provider keeps finding.
        let outcome = outcome.and_then(|()| match self.provider.standing_error() {
            Some(error) => Err(error.into()),
            None => Ok(()),
        });
        let now = (self.clock)();
        if let Err(error) = &outcome {
            tracing::warn!(account = %self.account.name, code = %error.code(), error = %error.message(), "a sync pass failed");
        }
        let state = state_for(&outcome, now);
        if let Err(error) = self
            .write(Batch {
                state,
                ..Batch::new(self.account_id)
            })
            .await
        {
            tracing::warn!(account = %self.account.name, error = %error.message(), "could not record the sync state");
        }
        let id = self.account_id;
        let counts = match self.read(move |store| store.counts(id)).await {
            Ok(counts) => Some(counts),
            Err(error) => {
                tracing::warn!(account = %self.account.name, code = %error.code(), error = %error.message(), "could not read the account's counts; the status keeps the last ones");
                None
            }
        };
        let next =
            now + i64::try_from(self.settings.poll_interval.as_secs()).unwrap_or(i64::MAX / 2);
        let paused = outcome.as_ref().err().is_some_and(SyncError::needs_auth);
        self.status(|s| {
            s.pass_running = false;
            s.passes_completed += 1;
            if let Some(counts) = counts {
                (s.messages, s.threads, s.bodies) =
                    (counts.messages, counts.threads, counts.bodies);
            }
            apply_outcome(s, &outcome, now, (!paused).then_some(next));
        });
        paused
    }
}

fn state_for(outcome: &Result<(), SyncError>, now: i64) -> StateUpdate {
    match outcome {
        Ok(()) => StateUpdate {
            phase: Some(SyncPhase::Idle.as_str().to_string()),
            last_sync_at: Some(now),
            last_error: Some(None),
            ..StateUpdate::default()
        },
        Err(error) => StateUpdate {
            phase: Some(
                if error.needs_auth() {
                    SyncPhase::NeedsAuth
                } else {
                    SyncPhase::Idle
                }
                .as_str()
                .to_string(),
            ),
            last_error: Some(Some((error.code(), now))),
            needs_auth: error.needs_auth().then_some(true),
            ..StateUpdate::default()
        },
    }
}

fn apply_outcome(
    s: &mut AccountSyncStatus,
    outcome: &Result<(), SyncError>,
    now: i64,
    next: Option<i64>,
) {
    s.next_attempt_at = next;
    match outcome {
        Ok(()) => {
            s.phase = SyncPhase::Idle;
            s.last_success_at = Some(now);
            s.last_error = None;
            s.last_pass_failed = false;
        }
        Err(error) => {
            s.needs_auth = error.needs_auth();
            s.phase = if s.needs_auth {
                SyncPhase::NeedsAuth
            } else {
                SyncPhase::Idle
            };
            s.last_pass_failed = true;
            s.last_error = Some(SyncErrorInfo {
                code: error.code(),
                class: Some(error.class()),
                message: error.message(),
                at: now,
                detail: error.detail().map(Untrusted::new),
            });
        }
    }
}
