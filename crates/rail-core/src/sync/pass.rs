//! The steps of a pass: labels, backfill (in `backfill.rs`), delta, bodies, prune.

use futures_util::{StreamExt, stream};

use super::backfill::needs_backfill;
use super::batch::{Batch, Prune, StateUpdate, Sweep};
use super::engine::{Engine, SyncError};
use super::{
    BODIES_PER_PASS, BODY_BATCH, BODY_BATCH_BYTES, BODY_CONCURRENCY, SEARCH_KEEP_DAYS,
    UNREADABLE_BODY,
};
use crate::config::DAY_SECS;
use crate::ops::SyncPhase;
use crate::provider::{Body, ProviderError, RemoteId, SyncCursor};
use crate::store::SyncStateRow;

/// Answers one delta step reads at most (each holds at most 5,000 added ids): 500,000 changes
/// a pass. The cursor is saved after every answer, so the next pass goes on from there.
const MAX_DELTA_ROUNDS: usize = 100;

impl Engine {
    pub(crate) async fn pass(&self) -> Result<(), SyncError> {
        self.status(|s| {
            s.pass_running = true;
            s.next_attempt_at = None;
            s.bodies_fetched = 0;
        });
        let now = (self.clock)();
        let window = self.settings.window_start_ms(now);
        self.labels().await?;
        self.envelopes(window).await?;
        self.bodies(now).await?;
        self.prune(now, window).await
    }

    /// Backfill and delta. With a cursor (a grown window, or such a backfill resumed), the
    /// delta runs first and the backfill lists at most `BACKFILL_PAGES_PER_PASS` pages with
    /// progress, so new
    /// mail arrives within one poll interval however long the backfill takes. Without one (the
    /// first full backfill), the delta can only follow it. A delta-first provider's grown
    /// window resyncs instead.
    async fn envelopes(&self, window: Option<i64>) -> Result<(), SyncError> {
        let state = self.state().await?;
        let backfill = needs_backfill(&state, window);
        if let Some(cursor) = state.cursor.clone() {
            // A delta-first provider's links keep the window they began with: a grown window
            // starts every folder over.
            if backfill && self.provider.backfills_by_delta() {
                return self.resync(window, "the window grew").await;
            }
            let resynced = self.catch_up(cursor, window).await?;
            if backfill && !resynced {
                self.backfill(&state, window).await?;
            }
            return Ok(());
        }
        if backfill {
            self.backfill(&state, window).await?;
        }
        if let Some(cursor) = self.state().await?.cursor {
            self.catch_up(cursor, window).await?;
        }
        Ok(())
    }

    /// The delta from `cursor`; an expired one resyncs in full. True when it resynced.
    async fn catch_up(&self, cursor: String, window: Option<i64>) -> Result<bool, SyncError> {
        match self.delta(cursor, window).await {
            Err(SyncError::Provider(ProviderError::CursorExpired)) => {
                self.resync(window, "the sync cursor expired").await?;
                Ok(true)
            }
            other => other.map(|()| false),
        }
    }

    async fn state(&self) -> Result<SyncStateRow, SyncError> {
        let id = self.account_id;
        self.read(move |store| store.sync_state(id)).await
    }

    async fn labels(&self) -> Result<(), SyncError> {
        self.phase(SyncPhase::Labels);
        let folders = self
            .call(SyncPhase::Labels, self.provider.folders())
            .await?;
        let batch = Batch {
            folders: Some(folders),
            state: StateUpdate {
                needs_auth: Some(false),
                ..StateUpdate::default()
            },
            ..Batch::new(self.account_id)
        };
        self.write(batch).await?;
        self.status(|s| s.needs_auth = false);
        Ok(())
    }

    /// Catch up from `cursor`, one batch per answer, while the provider says more waits; at
    /// most `MAX_DELTA_ROUNDS` answers a pass (the saved cursor lets the next pass go on).
    /// During a delta-first provider's first load the phase is `Backfill` and its count grows
    /// with what each answer adds; the answer that catches up carries the sweep the backfill
    /// left to it (`end_load`).
    async fn delta(&self, mut cursor: String, window: Option<i64>) -> Result<(), SyncError> {
        let load = self.first_load().await?;
        let phase = match load {
            Some(_) => SyncPhase::Backfill,
            None => SyncPhase::Delta,
        };
        self.phase(phase);
        for _ in 0..MAX_DELTA_ROUNDS {
            let asked = SyncCursor(cursor.clone());
            let set = self.call(phase, self.provider.changes(&asked)).await?;
            log_skipped(&self.account.name, &set.skipped);
            let (more, next, fetched) = (set.more, set.cursor.0.clone(), set.added.len() as u64);
            let mut batch = Batch {
                upserts: set.added,
                deletes: set.deleted,
                relabels: set.relabeled,
                state: StateUpdate {
                    cursor: Some(Some(next.clone())),
                    ..StateUpdate::default()
                },
                ..Batch::new(self.account_id)
            };
            if let (Some(started_at), false) = (load, more) {
                self.end_load(&mut batch, started_at, window, &set.cursor);
            }
            self.write(batch).await?;
            if load.is_some() {
                self.status(|s| s.backfill_fetched += fetched);
            }
            if !more {
                return Ok(());
            }
            if next == cursor {
                tracing::warn!(account = %self.account.name, "the provider said more changes wait but the cursor did not move; the next pass goes on");
                return Ok(());
            }
            cursor = next;
        }
        tracing::info!(account = %self.account.name, rounds = MAX_DELTA_ROUNDS, "many changes wait; the next pass goes on");
        Ok(())
    }

    /// The answer that caught a load up ends it, with the sweep when the provider walked every
    /// folder. Else the sweep would delete the mail of the folders it left, so the load ends
    /// without one: mail deleted at the provider meanwhile stays until a later full load.
    fn end_load(&self, batch: &mut Batch, started_at: i64, window: Option<i64>, at: &SyncCursor) {
        batch.state.backfill_started_at = Some(None);
        if !self.provider.walked_everything(at) {
            tracing::warn!(account = %self.account.name, "the provider left folders unwalked; the load ended without a sweep");
            return;
        }
        batch.sweep = Some(Sweep {
            started_at,
            window_start_ms: window,
        });
    }

    /// While a delta-first provider's first load runs: when it began, which its sweep keeps.
    async fn first_load(&self) -> Result<Option<i64>, SyncError> {
        if !self.provider.backfills_by_delta() {
            return Ok(None);
        }
        Ok(self.state().await?.backfill_started_at)
    }

    /// A cursor that cannot go on (`why` says so): forget it, backfill in full, then catch up
    /// once more.
    async fn resync(&self, window: Option<i64>, why: &str) -> Result<(), SyncError> {
        tracing::info!(account = %self.account.name, "{why}; resyncing in full");
        let clear = StateUpdate {
            cursor: Some(None),
            backfill_before: Some(None),
            backfill_cursor: Some(None),
            backfill_started_at: Some(None),
            backfill_resume: Some(None),
            ..StateUpdate::default()
        };
        self.write(Batch {
            state: clear,
            ..Batch::new(self.account_id)
        })
        .await?;
        let state = self.state().await?;
        self.backfill(&state, window).await?;
        match self.state().await?.cursor {
            Some(cursor) => self.delta(cursor, window).await,
            None => Ok(()),
        }
    }

    async fn bodies(&self, now: i64) -> Result<(), SyncError> {
        self.phase(SyncPhase::Bodies);
        let (id, since) = (self.account_id, self.settings.body_since_ms(now));
        let wanted = self
            .read(move |store| store.messages_wanting_bodies(id, since, BODIES_PER_PASS))
            .await?;
        let mut fetches = stream::iter(wanted)
            .map(|id| self.fetch_body(id))
            .buffer_unordered(BODY_CONCURRENCY);
        let mut pending = BodyBatch::default();
        while let Some(fetched) = fetches.next().await {
            if pending.push(fetched) {
                self.flush_bodies(std::mem::take(&mut pending)).await?;
            }
        }
        self.flush_bodies(pending).await
    }

    /// Write one batch of fetched bodies; then end the step if any fetch in it failed.
    async fn flush_bodies(&self, pending: BodyBatch) -> Result<(), SyncError> {
        if pending.fetched.is_empty() {
            return Ok(());
        }
        let (bodies, failure) = self.sort_bodies(pending.fetched);
        let count = bodies.len() as u64;
        self.write(Batch {
            bodies,
            ..Batch::new(self.account_id)
        })
        .await?;
        self.status(|s| s.bodies_fetched += count);
        match failure {
            Some(error) => Err(error.into()),
            None => Ok(()),
        }
    }

    async fn fetch_body(&self, id: RemoteId) -> (RemoteId, Result<Body, ProviderError>) {
        let result = self.call(SyncPhase::Bodies, self.provider.body(&id)).await;
        (id, result)
    }

    /// Bodies to store (a marker for unreadable ones; vanished ones left out) and the first
    /// failure that ends the step.
    fn sort_bodies(
        &self,
        fetched: Vec<(RemoteId, Result<Body, ProviderError>)>,
    ) -> (Vec<(RemoteId, Body)>, Option<ProviderError>) {
        let mut bodies = Vec::new();
        let mut failures = Vec::new();
        for (id, result) in fetched {
            match result {
                Ok(body) => bodies.push((id, body)),
                Err(ProviderError::NotFound) => {}
                Err(ProviderError::Permanent { .. }) => {
                    tracing::warn!(account = %self.account.name, id = %id.escape_debug(), "stored a marker for a body Rail could not read");
                    let marker = Body {
                        text: UNREADABLE_BODY.to_string(),
                        ..Body::default()
                    };
                    bodies.push((id, marker));
                }
                Err(error) => failures.push(error),
            }
        }
        (bodies, self.chunk_failure(failures))
    }

    /// The failure that ends the bodies step: a login failure before any other (it pauses the
    /// account); the codes of the rest are logged, never their detail.
    fn chunk_failure(&self, mut failures: Vec<ProviderError>) -> Option<ProviderError> {
        if failures.is_empty() {
            return None;
        }
        let first = failures
            .iter()
            .position(|e| matches!(e, ProviderError::NeedsAuth { .. }))
            .unwrap_or(0);
        let chosen = failures.remove(first);
        if !failures.is_empty() {
            let codes: Vec<&str> = failures.iter().map(ProviderError::code).collect();
            tracing::warn!(account = %self.account.name, chosen = chosen.code(), codes = %codes.join(","), "more body fetches failed in the same batch");
        }
        Some(chosen)
    }

    async fn prune(&self, now: i64, window: Option<i64>) -> Result<(), SyncError> {
        self.phase(SyncPhase::Prune);
        let covered = self.state().await?.backfill_window;
        let mut batch = Batch {
            prune: Some(Prune {
                window_start_ms: window,
                search_before: now - SEARCH_KEEP_DAYS * DAY_SECS,
            }),
            ..Batch::new(self.account_id)
        };
        // A shrunk window: the store now covers only it, so growing it again backfills.
        if let (Some(start), Some(covered)) = (window, covered)
            && start > covered
        {
            batch.state.backfill_window = Some(Some(start));
        }
        self.write(batch).await?;
        Ok(())
    }
}

/// Fetched bodies waiting for the writer: full at `BODY_BATCH` of them or at
/// `BODY_BATCH_BYTES` of text and HTML, so one batch never holds hundreds of MiB of mail.
#[derive(Default)]
pub(super) struct BodyBatch {
    fetched: Vec<(RemoteId, Result<Body, ProviderError>)>,
    bytes: usize,
}

impl BodyBatch {
    /// Add one fetch; true when the batch is now full.
    pub(super) fn push(&mut self, fetched: (RemoteId, Result<Body, ProviderError>)) -> bool {
        if let Ok(body) = &fetched.1 {
            self.bytes = self
                .bytes
                .saturating_add(body.text.len())
                .saturating_add(body.html.as_ref().map_or(0, String::len));
        }
        self.fetched.push(fetched);
        self.fetched.len() >= BODY_BATCH || self.bytes >= BODY_BATCH_BYTES
    }
}

pub(super) fn log_skipped(account: &str, skipped: &[RemoteId]) {
    if !skipped.is_empty() {
        let ids: Vec<String> = skipped
            .iter()
            .map(|id| id.escape_debug().to_string())
            .collect();
        tracing::warn!(account, count = skipped.len(), ids = %ids.join(","), "skipped messages Rail could not read; the next full backfill retries them");
    }
}
