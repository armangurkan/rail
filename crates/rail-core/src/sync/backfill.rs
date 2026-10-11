//! The backfill step of a pass (spec §4): list the window page by page, fetch metadata for the
//! ids this backfill has not stored yet, and commit each page with where the listing goes on.
//!
//! Gmail lists only roughly newest first (the live test saw spreads of hours, and imported
//! mail can be listed years out of place), and it silently accepts a page token it does not
//! know (the live test got a page for a made-up one), so neither a date nor a token's validity
//! can be relied on. A backfill goes on with Gmail's page token, stored after every listed
//! page; without a usable one it lists the window again from the top. Either way it skips the
//! metadata of ids it already stored, so its correctness depends on neither listing order nor
//! token behaviour: at worst a lost or stale token costs listing ids again (5 quota units per
//! 100). `backfill_before` is only a statistic and never bounds a listing.

use super::batch::{Batch, ResumeToken, StateUpdate, Sweep};
use super::engine::{Engine, SyncError};
use super::pass::log_skipped;
use super::{BACKFILL_MAX_PAGES_PER_PASS, BACKFILL_PAGES_PER_PASS, BACKFILL_TOKEN_MAX_AGE_SECS};
use crate::ops::SyncPhase;
use crate::provider::{EnvelopePage, IdPage, PageToken, ProviderError, RemoteId, Window};
use crate::store::SyncStateRow;

/// One second, in milliseconds: Gmail's `before:` resolution.
const SECOND_MS: i64 = 1000;

/// Whether this pass backfills: no cursor yet, an unfinished backfill, or a grown window.
pub(super) fn needs_backfill(state: &SyncStateRow, window: Option<i64>) -> bool {
    let wanted = window.unwrap_or(0);
    state.cursor.is_none()
        || state.backfill_before.is_some()
        || state.backfill_resume.is_some()
        || state.backfill_window.is_none_or(|covered| wanted < covered)
}

/// One listing request: the query, and the page of it to ask for.
#[derive(Debug, Clone)]
struct Listing {
    window: Window,
    page: Option<PageToken>,
}

/// What a running backfill carries from page to page.
struct Run {
    /// No cursor yet: the whole window is listed, and swept at the end.
    full: bool,
    /// The cursor read before a full backfill began.
    cursor: Option<String>,
    started: i64,
    /// The oldest receive time stored so far (`backfill_before`).
    marker: Option<i64>,
    /// The window start wanted now.
    window: Option<i64>,
    /// A grown window: the start the store already covers.
    covered: Option<i64>,
    /// This pass already gave up on a page token once and listed from the top.
    relisted: bool,
}

impl Run {
    /// Ids stored by this backfill are not fetched again: for a full one, sync rows synced
    /// since it began (older rows must be seen again, or the sweep deletes them); for a grown
    /// window, any sync row (the delta keeps those current).
    fn since(&self) -> Option<i64> {
        self.full.then_some(self.started)
    }

    /// The window's listing from the top, as a fresh backfill would ask for it: the window's
    /// start, and for a grown window its old start (the whole oldest second again). Never a
    /// bound from `backfill_before`.
    fn top_listing(&self) -> Listing {
        Listing {
            window: Window {
                after_ms: self.window,
                before_ms: self.covered.map(|c| c + SECOND_MS),
            },
            page: None,
        }
    }

    /// Whether this pass has done enough work; the next pass goes on from the token stored
    /// with the last page, so this bounds work and never loses the place. A capped
    /// (grown-window) backfill stops after `BACKFILL_PAGES_PER_PASS` pages with progress or
    /// `BACKFILL_MAX_PAGES_PER_PASS` in all; the first full backfill after
    /// `BACKFILL_MAX_PAGES_PER_PASS` pages without progress.
    fn pass_is_over(&self, progressed: usize, listed: usize) -> bool {
        if self.full {
            listed.saturating_sub(progressed) >= BACKFILL_MAX_PAGES_PER_PASS
        } else {
            progressed >= BACKFILL_PAGES_PER_PASS || listed >= BACKFILL_MAX_PAGES_PER_PASS
        }
    }
}

/// The stored listing, when the backfill can go on with its token: issued within
/// `BACKFILL_TOKEN_MAX_AGE_SECS`, for a listing reaching back at least as far as the window
/// wants now (its query is used unchanged: a token is only valid for its own query). A window
/// grown past it drops the token. The window's start moves with the clock when `history_days`
/// is set, so "reaches back as far" rather than "the same": an older start only lists mail the
/// prune step then drops.
fn resumable(state: &SyncStateRow, window: Option<i64>, now: i64) -> Option<Listing> {
    let resume = state.backfill_resume.as_ref()?;
    let fresh = now.saturating_sub(resume.issued_at) <= BACKFILL_TOKEN_MAX_AGE_SECS;
    let reaches = match (resume.listing.after_ms, window) {
        (None, _) => true,
        (Some(_), None) => false,
        (Some(listed), Some(wanted)) => listed <= wanted,
    };
    (fresh && reaches).then(|| Listing {
        window: resume.listing,
        page: Some(resume.token.clone()),
    })
}

/// Failures a stale or unknown page token could cause: a refusal, a 404, any other permanent
/// refusal. Transient trouble and a login failure are not about the token.
fn token_trouble(error: &ProviderError) -> bool {
    matches!(
        error,
        ProviderError::PageTokenRejected
            | ProviderError::NotFound
            | ProviderError::Permanent { .. }
    )
}

impl Engine {
    pub(super) async fn backfill(
        &self,
        state: &SyncStateRow,
        window: Option<i64>,
    ) -> Result<(), SyncError> {
        self.phase(SyncPhase::Backfill);
        let full = state.cursor.is_none();
        let (cursor, started) = if full {
            self.first_cursor(state).await?
        } else {
            (None, (self.clock)())
        };
        let mut run = Run {
            full,
            cursor,
            started,
            marker: state.backfill_before,
            window,
            covered: state.backfill_window.filter(|_| !full),
            relisted: false,
        };
        let mut listing =
            resumable(state, window, (self.clock)()).unwrap_or_else(|| run.top_listing());
        let (mut progressed, mut listed) = (0, 0);
        loop {
            listed += 1;
            let Some(page) = self.list(&mut run, &listing).await? else {
                listing = run.top_listing();
                continue;
            };
            let next = page.next.clone();
            progressed += usize::from(self.commit_page(&mut run, &listing, page).await?);
            let Some(next) = next else { break };
            if run.pass_is_over(progressed, listed) {
                tracing::info!(account = %self.account.name, pages = listed, progressed, "the backfill goes on next pass");
                return Ok(());
            }
            listing.page = Some(next);
        }
        self.end_backfill(run).await
    }

    /// One page of `listing`; `None` when a request carrying a page token failed in a way a
    /// stale token could explain (the caller lists from the top without one), once a pass.
    /// Gmail is not known to refuse a bad token (it answered a made-up one), so this is a
    /// defensive branch; a second such failure, or any failure without a token, ends the pass.
    async fn list(&self, run: &mut Run, listing: &Listing) -> Result<Option<IdPage>, SyncError> {
        let asked = self.provider.list(&listing.window, listing.page.as_ref());
        match self.call(SyncPhase::Backfill, asked).await {
            Err(error) if listing.page.is_some() && !run.relisted && token_trouble(&error) => {
                tracing::info!(account = %self.account.name, code = error.code(), "a listing with a page token failed; listing the window again from the top, skipping what is stored");
                run.relisted = true;
                Ok(None)
            }
            other => Ok(Some(other?)),
        }
    }

    /// Fetch metadata for the page's ids this backfill has not stored, and commit them with
    /// the marker and where the listing goes on, in one batch: a crash never leaves a token
    /// pointing past uncommitted mail. True when the page stored a message (progress; the
    /// marker only ever moves with one).
    async fn commit_page(
        &self,
        run: &mut Run,
        listing: &Listing,
        page: IdPage,
    ) -> Result<bool, SyncError> {
        let wanted = self.unstored(run.since(), page.ids).await?;
        let fetched = if wanted.is_empty() {
            EnvelopePage::default()
        } else {
            self.call(SyncPhase::Backfill, self.provider.envelopes(&wanted))
                .await?
        };
        log_skipped(&self.account.name, &fetched.skipped);
        let count = fetched.envelopes.len() as u64;
        let oldest = fetched.envelopes.iter().map(|e| e.internal_date).min();
        let marker = match (run.marker, oldest) {
            (Some(marker), Some(oldest)) => Some(marker.min(oldest)),
            (marker, oldest) => marker.or(oldest),
        };
        let resume = page.next.map(|token| ResumeToken {
            token,
            listing: listing.window,
            issued_at: (self.clock)(),
        });
        let batch = Batch {
            upserts: fetched.envelopes,
            state: StateUpdate {
                phase: Some(SyncPhase::Backfill.as_str().to_string()),
                backfill_before: marker.map(Some),
                backfill_cursor: run.cursor.clone().map(Some),
                backfill_started_at: run.full.then_some(Some(run.started)),
                backfill_resume: Some(resume),
                ..StateUpdate::default()
            },
            ..Batch::new(self.account_id)
        };
        self.write(batch).await?;
        run.marker = marker;
        self.status(|s| s.backfill_fetched += count);
        Ok(count > 0)
    }

    /// `ids` without those this backfill already stored (one store query).
    async fn unstored(
        &self,
        since: Option<i64>,
        ids: Vec<RemoteId>,
    ) -> Result<Vec<RemoteId>, SyncError> {
        let (account, asked) = (self.account_id, ids.clone());
        let known = self
            .read(move |store| store.backfilled_ids(account, &asked, since))
            .await?;
        Ok(ids.into_iter().filter(|id| !known.contains(id)).collect())
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

    async fn end_backfill(&self, run: Run) -> Result<(), SyncError> {
        let mut state = StateUpdate {
            backfill_before: Some(None),
            backfill_cursor: Some(None),
            backfill_started_at: Some(None),
            backfill_resume: Some(None),
            backfill_window: Some(Some(run.window.unwrap_or(0))),
            ..StateUpdate::default()
        };
        if run.full {
            state.cursor = Some(run.cursor);
        }
        let batch = Batch {
            state,
            sweep: run.full.then_some(Sweep {
                started_at: run.started,
                window_start_ms: run.window,
            }),
            ..Batch::new(self.account_id)
        };
        self.write(batch).await?;
        Ok(())
    }
}
