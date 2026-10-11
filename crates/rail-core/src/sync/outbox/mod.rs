//! One account's outbox worker, run by `SyncManager` beside the engine: it claims the oldest
//! due row, sends it through the account's provider (outside the engine's retry status), and
//! completes or fails it through the writer. It sleeps until the earliest `not_before` (at
//! least `MIN_IDLE`, at most `MAX_IDLE`) or the new-row signal; it pauses while the account
//! needs a login; stopping drops a call in flight (the row stays inflight, and the restart rule
//! sends it again). Logs carry row ids, counts and codes only.

mod plan;
#[cfg(test)]
mod tests;

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::{Notify, watch};

use self::plan::{Plan, Remote};
use super::batch::{Batch, StateUpdate};
use super::engine::{Clock, stopped};
use super::manager::{retry_delay, unobserved};
use super::status::StatusBoard;
use super::writer::WriterHandle;
use crate::error::ErrorClass;
use crate::ops::{ActionFailed, ErrorCode, OpError, OutboxDepth, SyncPhase};
use crate::provider::{Provider, ProviderError};
use crate::store::outbox::{Failure, OutboxOutcome, OutboxRow, OutboxWrite};
use crate::store::{Store, StoreError};

/// The shortest and longest idle sleep.
pub const MIN_IDLE: Duration = Duration::from_secs(1);
pub const MAX_IDLE: Duration = Duration::from_secs(60);

pub struct WorkerParts {
    pub name: String,
    pub address: String,
    pub account_id: i64,
    pub provider: Arc<dyn Provider>,
    pub writer: WriterHandle,
    /// A `Store::open_reader` connection of its own.
    pub reader: Store,
    pub board: StatusBoard,
    pub signal: Arc<Notify>,
    pub clock: Clock,
}

pub struct Worker {
    name: String,
    address: String,
    account_id: i64,
    provider: Arc<dyn Provider>,
    writer: WriterHandle,
    reader: Arc<Mutex<Store>>,
    board: StatusBoard,
    signal: Arc<Notify>,
    clock: Clock,
}

/// What the loop does next.
pub(super) enum Next {
    Again,
    Idle(Duration),
}

/// Set the account's `outbox` depth on the board from `store`.
pub fn publish_depth(board: &StatusBoard, address: &str, store: &Store, account_id: i64) {
    match store.outbox_depth(account_id) {
        Ok((pending, failed)) => {
            board.update(address, |s| s.outbox = OutboxDepth { pending, failed })
        }
        Err(error) => {
            tracing::warn!(account = account_id, error = %error, "could not read the outbox depth")
        }
    }
}

impl Worker {
    pub fn new(parts: WorkerParts) -> Self {
        Self {
            name: parts.name,
            address: parts.address,
            account_id: parts.account_id,
            provider: parts.provider,
            writer: parts.writer,
            reader: Arc::new(Mutex::new(parts.reader)),
            board: parts.board,
            signal: parts.signal,
            clock: parts.clock,
        }
    }

    /// Recover inflight rows, then send until `stop` turns true or its sender goes.
    pub async fn run(self, mut stop: watch::Receiver<bool>) {
        if let Err(error) = self
            .writer
            .outbox(OutboxWrite::Recover {
                account_id: self.account_id,
            })
            .await
        {
            tracing::warn!(account = %self.name, error = %error.message, "could not recover the outbox");
        }
        self.publish().await;
        loop {
            if self.paused() {
                tokio::select! {
                    () = self.signal.notified() => continue,
                    () = stopped(&mut stop) => break,
                }
            }
            let next = tokio::select! {
                next = self.step() => next,
                () = stopped(&mut stop) => break,
            };
            if let Next::Idle(wait) = next {
                tokio::select! {
                    () = tokio::time::sleep(wait) => {}
                    () = self.signal.notified() => {}
                    () = stopped(&mut stop) => break,
                }
            }
        }
    }

    fn paused(&self) -> bool {
        self.board.get(&self.address).is_some_and(|s| s.needs_auth)
    }

    async fn read<T: Send + 'static>(
        &self,
        read: impl FnOnce(&Store) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, OpError> {
        let reader = self.reader.clone();
        tokio::task::spawn_blocking(move || {
            read(&reader.lock().unwrap_or_else(PoisonError::into_inner))
        })
        .await
        .map_err(|_| OpError::internal("an outbox read failed to run"))?
        .map_err(OpError::from)
    }

    async fn publish(&self) {
        let (board, address, id) = (self.board.clone(), self.address.clone(), self.account_id);
        let _ = self
            .read(move |store| {
                publish_depth(&board, &address, store, id);
                Ok(())
            })
            .await;
    }

    /// Claim and send one row; or how long to sleep.
    pub(super) async fn step(&self) -> Next {
        let claimed = self
            .writer
            .outbox(OutboxWrite::Claim {
                account_id: self.account_id,
            })
            .await;
        let row = match claimed {
            Ok(OutboxOutcome::Claimed(Some(row))) => row,
            Ok(_) => return Next::Idle(self.idle().await),
            Err(error) => {
                tracing::warn!(account = %self.name, error = %error.message, "could not claim an outbox row");
                return Next::Idle(MAX_IDLE);
            }
        };
        let result = self.send(&row).await;
        self.settle(row, result).await;
        self.publish().await;
        Next::Again
    }

    /// Until the earliest due row, within `MIN_IDLE..=MAX_IDLE`.
    async fn idle(&self) -> Duration {
        let id = self.account_id;
        let due = self
            .read(move |store| store.outbox_next_due(id))
            .await
            .ok()
            .flatten();
        let now = (self.clock)();
        due.map_or(MAX_IDLE, |at| {
            Duration::from_secs(u64::try_from(at - now).unwrap_or(0))
        })
        .clamp(MIN_IDLE, MAX_IDLE)
    }

    async fn send(&self, row: &OutboxRow) -> Result<(), ProviderError> {
        let target = row.clone();
        let planned = self.read(move |store| plan::plan(store, &target)).await;
        let planned = planned.map_err(|e| ProviderError::transient(&e.message))?;
        match planned {
            Plan::Nothing => Ok(()),
            Plan::LabelGone => Err(ProviderError::permanent(
                "a label of this action no longer exists in Gmail",
            )),
            Plan::Send { threads, change } => unobserved(self.call_each(&threads, &change)).await,
        }
    }

    /// One provider call per thread, stopping at the first error: a failure part-way fails the
    /// whole row (threads Gmail already changed come back through the next sync).
    async fn call_each(&self, threads: &[String], change: &Remote) -> Result<(), ProviderError> {
        for thread in threads {
            let one = std::slice::from_ref(thread);
            match change {
                Remote::Trash => self.provider.trash_threads(one).await?,
                Remote::Modify { add, remove } => {
                    self.provider.modify_threads(one, add, remove).await?;
                }
            }
        }
        Ok(())
    }

    /// The write that records `result` for `row`.
    async fn write_for(&self, row: &OutboxRow, result: &Result<(), ProviderError>) -> OutboxWrite {
        match result {
            Ok(()) => OutboxWrite::Complete { id: row.id },
            Err(ProviderError::NeedsAuth { .. }) => {
                self.needs_login().await;
                OutboxWrite::Release { id: row.id }
            }
            Err(error @ ProviderError::Transient { .. }) => {
                let wait = retry_delay(row.attempts.saturating_add(1));
                let retry_at = (self.clock)() + i64::try_from(wait.as_secs()).unwrap_or(900);
                OutboxWrite::Fail {
                    id: row.id,
                    failure: Failure::Transient {
                        error: words(error),
                        retry_at,
                    },
                }
            }
            Err(error) => OutboxWrite::Fail {
                id: row.id,
                failure: Failure::Permanent(words(error)),
            },
        }
    }

    async fn settle(&self, row: OutboxRow, result: Result<(), ProviderError>) {
        let write = self.write_for(&row, &result).await;
        tracing::info!(account = %self.name, row = row.id, threads = row.thread_ids.len(), outcome = result.as_ref().err().map_or("sent", ProviderError::code), "outbox row");
        match self.writer.outbox(write).await {
            Ok(OutboxOutcome::Failed {
                gave_up: true,
                row: Some(failed),
            }) => self.notify(failed, result.err()),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(account = %self.name, row = row.id, error = %error.message, "could not record an outbox result")
            }
        }
    }

    /// The account needs a login: the status and the store say so, as a failed pass would.
    async fn needs_login(&self) {
        self.board.update(&self.address, |s| {
            s.needs_auth = true;
            s.phase = SyncPhase::NeedsAuth;
        });
        let state = StateUpdate {
            needs_auth: Some(true),
            ..StateUpdate::default()
        };
        if let Err(error) = self
            .writer
            .write(Batch {
                state,
                ..Batch::new(self.account_id)
            })
            .await
        {
            tracing::warn!(account = %self.name, error = %error.message, "could not record needs_auth");
        }
    }

    fn notify(&self, row: OutboxRow, cause: Option<ProviderError>) {
        let mut error = match cause {
            Some(cause) => OpError::from(cause),
            None => OpError::new(
                ErrorCode::Provider,
                ErrorClass::Permanent,
                "Gmail refused the change",
            ),
        };
        if let Some(last) = row
            .last_error
            .as_deref()
            .filter(|e| e.starts_with("gave up"))
        {
            error.message = last.split(':').next().unwrap_or(last).to_string();
        }
        self.board.report_failure(&ActionFailed {
            account: self.name.clone(),
            row: row.id,
            threads: row.thread_ids,
            action: row.action,
            error,
        });
    }
}

/// Rail's words for a failure, with the provider's after a colon (kept in `last_error`).
fn words(error: &ProviderError) -> String {
    match error.detail() {
        Some(detail) => format!("{error}: {detail}"),
        None => error.to_string(),
    }
}
