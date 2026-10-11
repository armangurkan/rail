//! The steps of a pass (spec §4): labels, backfill, delta, bodies, prune.

use futures_util::{StreamExt, stream};

use super::batch::{Batch, Prune, StateUpdate, Sweep};
use super::engine::{Engine, SyncError};
use super::{BODIES_PER_PASS, BODY_BATCH, BODY_CONCURRENCY, SEARCH_KEEP_DAYS, UNREADABLE_BODY};
use crate::config::DAY_SECS;
use crate::ops::SyncPhase;
use crate::provider::{Body, PageToken, ProviderError, RemoteId, SyncCursor, Window};
use crate::store::SyncStateRow;

/// One second, in milliseconds: Gmail's `before:` resolution.
const SECOND_MS: i64 = 1000;
/// Answers one delta step reads at most (each holds at most 5,000 added ids): 500,000 changes
/// a pass. The cursor is saved after every answer, so the next pass goes on from there.
const MAX_DELTA_ROUNDS: usize = 100;

/// Whether this pass backfills: no cursor yet, an unfinished backfill, or a grown window.
fn needs_backfill(state: &SyncStateRow, window: Option<i64>) -> bool {
    let wanted = window.unwrap_or(0);
    state.cursor.is_none()
        || state.backfill_before.is_some()
        || state.backfill_window.is_none_or(|covered| wanted < covered)
}

/// The listing a backfill resumes or starts: below the marker (the whole oldest second
/// again), below the old window when it grew, or from the top.
fn listing(state: &SyncStateRow, window: Option<i64>) -> Window {
    let before = match (
        state.backfill_before,
        state.cursor.is_some(),
        state.backfill_window,
    ) {
        (Some(marker), _, _) => Some(marker + SECOND_MS),
        (None, true, Some(covered)) => Some(covered + SECOND_MS),
        _ => None,
    };
    Window {
        after_ms: window,
        before_ms: before,
    }
}

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
        let state = self.state().await?;
        if needs_backfill(&state, window) {
            self.backfill(&state, window).await?;
        }
        if let Some(cursor) = self.state().await?.cursor {
            match self.delta(cursor).await {
                Err(SyncError::Provider(ProviderError::CursorExpired)) => {
                    self.resync(window).await?
                }
                other => other?,
            }
        }
        self.bodies(now).await?;
        self.prune(now, window).await
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

    async fn backfill(&self, state: &SyncStateRow, window: Option<i64>) -> Result<(), SyncError> {
        self.phase(SyncPhase::Backfill);
        let full = state.cursor.is_none();
        let (cursor, started) = if full {
            self.first_cursor(state).await?
        } else {
            (None, (self.clock)())
        };
        let listing = listing(state, window);
        let mut page: Option<PageToken> = None;
        loop {
            let fetched = self
                .call(
                    SyncPhase::Backfill,
                    self.provider.backfill(&listing, page.as_ref()),
                )
                .await?;
            log_skipped(&self.account.name, &fetched.skipped);
            let count = fetched.envelopes.len() as u64;
            let oldest = fetched.envelopes.iter().map(|e| e.internal_date).min();
            let batch = Batch {
                upserts: fetched.envelopes,
                state: StateUpdate {
                    phase: Some(SyncPhase::Backfill.as_str().to_string()),
                    backfill_before: oldest.map(Some),
                    backfill_cursor: cursor.clone().map(Some),
                    backfill_started_at: full.then_some(Some(started)),
                    ..StateUpdate::default()
                },
                ..Batch::new(self.account_id)
            };
            self.write(batch).await?;
            self.status(|s| s.backfill_fetched += count);
            match fetched.next {
                Some(next) => page = Some(next),
                None => break,
            }
        }
        self.end_backfill(full, cursor, started, window).await
    }

    /// The cursor read before a full backfill's first page, and when it began: from the store
    /// when the backfill is being resumed, else from `getProfile` now.
    async fn first_cursor(&self, state: &SyncStateRow) -> Result<(Option<String>, i64), SyncError> {
        if let (Some(cursor), Some(started)) = (&state.backfill_cursor, state.backfill_started_at) {
            return Ok((Some(cursor.clone()), started));
        }
        let profile = self
            .call(SyncPhase::Backfill, self.provider.profile())
            .await?;
        self.status(|s| {
            s.backfill_fetched = 0;
            s.backfill_estimate = Some(profile.messages_total);
        });
        Ok((Some(profile.cursor.0), (self.clock)()))
    }

    async fn end_backfill(
        &self,
        full: bool,
        cursor: Option<String>,
        started: i64,
        window: Option<i64>,
    ) -> Result<(), SyncError> {
        let mut state = StateUpdate {
            backfill_before: Some(None),
            backfill_cursor: Some(None),
            backfill_started_at: Some(None),
            backfill_window: Some(Some(window.unwrap_or(0))),
            ..StateUpdate::default()
        };
        if full {
            state.cursor = Some(cursor);
        }
        let batch = Batch {
            state,
            sweep: full.then_some(Sweep {
                started_at: started,
                window_start_ms: window,
            }),
            ..Batch::new(self.account_id)
        };
        self.write(batch).await?;
        Ok(())
    }

    /// Catch up from `cursor`, one batch per answer, while Gmail says more waits; at most
    /// `MAX_DELTA_ROUNDS` answers a pass (the saved cursor lets the next pass go on).
    async fn delta(&self, mut cursor: String) -> Result<(), SyncError> {
        self.phase(SyncPhase::Delta);
        for _ in 0..MAX_DELTA_ROUNDS {
            let asked = SyncCursor(cursor.clone());
            let set = self
                .call(SyncPhase::Delta, self.provider.changes(&asked))
                .await?;
            log_skipped(&self.account.name, &set.skipped);
            let (more, next) = (set.more, set.cursor.0.clone());
            let batch = Batch {
                upserts: set.added,
                deletes: set.deleted,
                relabels: set.relabeled,
                state: StateUpdate {
                    cursor: Some(Some(next.clone())),
                    ..StateUpdate::default()
                },
                ..Batch::new(self.account_id)
            };
            self.write(batch).await?;
            if !more {
                return Ok(());
            }
            if next == cursor {
                tracing::warn!(account = %self.account.name, "Gmail said more changes wait but the cursor did not move; the next pass goes on");
                return Ok(());
            }
            cursor = next;
        }
        tracing::info!(account = %self.account.name, rounds = MAX_DELTA_ROUNDS, "many changes wait; the next pass goes on");
        Ok(())
    }

    /// `CursorExpired`: forget the cursor, backfill in full, then catch up once more.
    async fn resync(&self, window: Option<i64>) -> Result<(), SyncError> {
        tracing::info!(account = %self.account.name, "the sync cursor expired; resyncing in full");
        let clear = StateUpdate {
            cursor: Some(None),
            backfill_before: Some(None),
            backfill_cursor: Some(None),
            backfill_started_at: Some(None),
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
            Some(cursor) => self.delta(cursor).await,
            None => Ok(()),
        }
    }

    async fn bodies(&self, now: i64) -> Result<(), SyncError> {
        self.phase(SyncPhase::Bodies);
        let (id, since) = (self.account_id, self.settings.body_since_ms(now));
        let wanted = self
            .read(move |store| store.messages_wanting_bodies(id, since, BODIES_PER_PASS))
            .await?;
        for chunk in wanted.chunks(BODY_BATCH) {
            let fetched: Vec<(RemoteId, Result<Body, ProviderError>)> =
                stream::iter(chunk.iter().cloned())
                    .map(|id| self.fetch_body(id))
                    .buffer_unordered(BODY_CONCURRENCY)
                    .collect()
                    .await;
            let (bodies, failure) = self.sort_bodies(fetched);
            let count = bodies.len() as u64;
            self.write(Batch {
                bodies,
                ..Batch::new(self.account_id)
            })
            .await?;
            self.status(|s| s.bodies_fetched += count);
            if let Some(error) = failure {
                return Err(error.into());
            }
        }
        Ok(())
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
        let mut failure = None;
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
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        (bodies, failure)
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

fn log_skipped(account: &str, skipped: &[RemoteId]) {
    if !skipped.is_empty() {
        let ids: Vec<String> = skipped
            .iter()
            .map(|id| id.escape_debug().to_string())
            .collect();
        tracing::warn!(account, count = skipped.len(), ids = %ids.join(","), "skipped messages Rail could not read; the next full backfill retries them");
    }
}
