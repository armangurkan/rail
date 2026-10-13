//! The one live test: read-only calls against a real Gmail account, run by hand with
//! `RAIL_GMAIL_LIVE_ACCOUNT=<name> cargo test -p rail-core -- --ignored gmail_live`.
//! It uses your real config and token store (the `RAIL_*_DIR` variables apply), sends only
//! GET requests to the Gmail API (so it never changes the mailbox) and may refresh your stored
//! tokens, which is a POST to Google's token endpoint. Without `RAIL_GMAIL_LIVE_ACCOUNT` it
//! prints a note and passes. Failure messages carry counts and Rail's own words, never mail
//! content or Gmail's error text, so they are safe to paste. It confirms the Gmail shapes
//! this phase assumed:
//!
//! - `after:` and `before:` take epoch seconds;
//! - listings come only roughly newest first: the test reports the largest out-of-order spread
//!   it saw (in hours, a number only) instead of asserting an order, since Rail no longer
//!   relies on one (a backfill resumes from Gmail's page token);
//! - a page token round-trips: the second page answers and repeats no id of the first; and a
//!   made-up token is reported as Gmail answers it (it was silently accepted when last run;
//!   Rail re-lists rather than relying on a rejection);
//! - a listing with no match answers without a `messages` key and Rail reads it as empty;
//! - subjects hold no raw encoded words and snippets no `&#39;`-style references;
//! - bodies decode into text (base64url with or without padding is covered by the unit tests
//!   only: this test cannot see the raw `data`);
//! - a very old `startHistoryId` is refused with HTTP 404, which Rail reports as
//!   `CursorExpired`.
//!
//! The last one is an assumption about Google's service, not about Rail. If that assertion
//! fails, Gmail answered an old history id some other way: revisit the assumption (and
//! `history.rs`'s mapping of the 404), do not assume Rail is broken. A check that needs mail
//! the account does not have (no mail in the last 30 days, no encoded subject) is reported on
//! standard error and not counted as confirmed.

use std::sync::Arc;

use crate::auth::{ItemIdCache, unix_now};
use crate::config::{self, DAY_SECS};
use crate::paths::Paths;
use crate::provider::{
    Envelope, FolderKind, PageToken, Provider, ProviderError, SyncCursor, Window,
};
use crate::sync::manager::ProviderRequest;
use crate::sync::providers::gmail_for;

/// How many of the listed messages have their bodies fetched.
const BODIES_CHECKED: usize = 5;

/// Rail's own words for a failed call: the error's message, never Gmail's detail.
fn ok<T>(result: Result<T, ProviderError>, what: &str) -> T {
    match result {
        Ok(value) => value,
        Err(e) => panic!("{what} failed: {e}"),
    }
}

fn live_provider() -> Option<Arc<dyn Provider>> {
    let Ok(who) = std::env::var("RAIL_GMAIL_LIVE_ACCOUNT") else {
        eprintln!("SKIPPED: set RAIL_GMAIL_LIVE_ACCOUNT to an account name to run this test");
        return None;
    };
    let paths = Paths::from_env().expect("a home directory");
    let config = config::load(&paths.config_file()).expect("config.toml loads");
    let account = ok_op(crate::accounts::find_account(&config, &who)).clone();
    Some(ok_op(gmail_for(ProviderRequest {
        paths,
        config,
        account,
        ids: ItemIdCache::default(),
        token_url: None,
        gmail_url: None,
        graph_url: None,
        observer: Arc::new(|wait| eprintln!("retrying in {wait:?}")),
    })))
}

fn ok_op<T>(result: Result<T, crate::ops::OpError>) -> T {
    match result {
        Ok(value) => value,
        Err(e) => panic!("building the provider failed: {}", e.message),
    }
}

async fn check_labels_and_profile(gmail: &dyn Provider) -> SyncCursor {
    let folders = ok(gmail.folders().await, "labels");
    assert!(
        folders.iter().any(|f| f.kind == FolderKind::Inbox),
        "INBOX maps to the inbox"
    );
    assert!(
        folders
            .iter()
            .all(|f| f.remote_id != "UNREAD" && f.remote_id != "STARRED"),
        "flag labels are not folders"
    );
    let profile = ok(gmail.profile().await, "profile");
    assert!(
        !profile.cursor.0.is_empty() && profile.cursor.0.bytes().all(|b| b.is_ascii_digit()),
        "historyId is numeric"
    );
    profile.cursor
}

/// The largest amount, in hours, by which a message listed later was newer than one listed
/// before it (0 when the listing is newest first).
fn out_of_order_hours(dates: &[i64]) -> f64 {
    let mut oldest_so_far = i64::MAX;
    let mut spread = 0_i64;
    for &date in dates {
        if oldest_so_far != i64::MAX {
            spread = spread.max(date - oldest_so_far);
        }
        oldest_so_far = oldest_so_far.min(date);
    }
    spread as f64 / 3_600_000.0
}

/// Lists the last 30 days (ending a day ago, so both `after:` and `before:` are used), checks
/// the window and reports how far out of order the listing is.
async fn check_window_and_order(gmail: &dyn Provider) -> Vec<Envelope> {
    let now = unix_now();
    let window = Window {
        after_ms: Some((now - 30 * DAY_SECS) * 1000),
        before_ms: Some((now - DAY_SECS) * 1000),
    };
    let page = ok(gmail.list(&window, None).await, "listing");
    let found = ok(gmail.envelopes(&page.ids).await, "metadata");
    let dates: Vec<i64> = found.envelopes.iter().map(|e| e.internal_date).collect();
    let outside = dates.iter().filter(|d| !window.contains(**d)).count();
    assert_eq!(
        outside,
        0,
        "epoch-second after:/before: bound the listing ({outside} of {} outside)",
        dates.len()
    );
    eprintln!(
        "listing order: {} messages, largest out-of-order spread {:.2} hours (Rail does not rely on order)",
        dates.len(),
        out_of_order_hours(&dates)
    );
    if dates.is_empty() {
        eprintln!("NOT CONFIRMED: no mail 1 to 30 days old; window and text checks skipped");
    }
    found.envelopes
}

/// The whole mailbox's first page hands a token the second page answers to, with no id of the
/// first again; a made-up token is reported as Gmail answers it.
async fn check_page_tokens(gmail: &dyn Provider) {
    let whole = Window::default();
    let first = ok(gmail.list(&whole, None).await, "first page");
    match &first.next {
        Some(token) => {
            let second = ok(gmail.list(&whole, Some(token)).await, "second page");
            let repeated = second
                .ids
                .iter()
                .filter(|id| first.ids.contains(id))
                .count();
            assert_eq!(repeated, 0, "the second page repeats {repeated} ids");
            eprintln!(
                "page tokens round-trip: yes ({} then {} ids)",
                first.ids.len(),
                second.ids.len()
            );
        }
        None => eprintln!("NOT CONFIRMED: the mailbox has one page only; page tokens not tried"),
    }
    let made_up = PageToken("13371337133713371337".into());
    match gmail.list(&whole, Some(&made_up)).await {
        Ok(_) => eprintln!(
            "a made-up page token: silently accepted (Rail re-lists rather than relying on rejection)"
        ),
        Err(e) => eprintln!("a made-up page token: refused ({})", e.code()),
    }
}

/// A window in the future lists nothing: the answer has no `messages` key.
async fn check_empty_listing(gmail: &dyn Provider) {
    let future = Window {
        after_ms: Some((unix_now() + DAY_SECS) * 1000),
        before_ms: None,
    };
    let page = ok(gmail.list(&future, None).await, "empty listing");
    assert!(
        page.ids.is_empty() && page.next.is_none(),
        "empty list reads as empty"
    );
    let none = ok(
        gmail
            .search("rail-live-test-no-such-word-0f3a9c71", 5)
            .await,
        "empty search",
    );
    assert!(
        none.is_empty(),
        "a search with no match reads as empty ({} ids)",
        none.len()
    );
}

async fn check_text(gmail: &dyn Provider, envelopes: &[Envelope]) {
    let mut encoded_seen = 0;
    let mut entity_seen = 0;
    let mut bodies = 0;
    for (i, envelope) in envelopes.iter().enumerate() {
        assert!(
            !envelope.subject.contains("=?"),
            "subject {i} still holds an encoded word ({} bytes)",
            envelope.subject.len()
        );
        assert!(
            !envelope.snippet.contains("&#39;"),
            "snippet {i} still holds a character reference ({} bytes)",
            envelope.snippet.len()
        );
        encoded_seen += usize::from(!envelope.subject.is_ascii());
        entity_seen += usize::from(envelope.snippet.contains('\''));
    }
    for envelope in envelopes.iter().take(BODIES_CHECKED) {
        let body = ok(gmail.body(&envelope.remote_id).await, "body");
        assert!(
            !body.text.is_empty() || !body.attachments.is_empty() || body.html.is_some(),
            "a body decodes to text, attachments or HTML"
        );
        bodies += 1;
    }
    eprintln!(
        "checked {} envelopes: {encoded_seen} with a non-ASCII subject (decoded words), \
         {entity_seen} snippets with an apostrophe (decoded reference), {bodies} bodies decoded",
        envelopes.len()
    );
    if encoded_seen == 0 || entity_seen == 0 {
        eprintln!("NOT CONFIRMED: no sampled mail exercised encoded words or snippet references");
    }
}

/// The current history id answers; a very old one is refused with 404, read as `CursorExpired`.
/// If the second assertion fails, Google answers old history ids differently from what the
/// plan assumed: revisit the assumption; Rail is not necessarily broken.
async fn check_history(gmail: &dyn Provider, current: &SyncCursor) {
    let changes = ok(gmail.changes(current).await, "history");
    assert!(!changes.cursor.0.is_empty());
    let expired = gmail.changes(&SyncCursor("1".into())).await;
    let seen = match &expired {
        Ok(_) => "a change set",
        Err(ProviderError::CursorExpired) => "CursorExpired",
        Err(ProviderError::NotFound) => "NotFound",
        Err(ProviderError::NeedsAuth { .. }) => "NeedsAuth",
        Err(ProviderError::Transient { .. }) => "a transient error",
        Err(ProviderError::Permanent { .. }) => "a permanent error",
        Err(ProviderError::PageTokenRejected) => "PageTokenRejected",
    };
    eprintln!("a very old historyId gives: {seen} (expected CursorExpired)");
    assert!(
        matches!(expired, Err(ProviderError::CursorExpired)),
        "a very old historyId gave {seen}: revisit the 404 assumption"
    );
}

#[tokio::test]
#[ignore = "talks to a real Gmail account; run by hand"]
async fn gmail_live_read_only() {
    let Some(gmail) = live_provider() else {
        return;
    };
    let cursor = check_labels_and_profile(gmail.as_ref()).await;
    let envelopes = check_window_and_order(gmail.as_ref()).await;
    check_page_tokens(gmail.as_ref()).await;
    check_empty_listing(gmail.as_ref()).await;
    check_text(gmail.as_ref(), &envelopes).await;
    check_history(gmail.as_ref(), &cursor).await;
    let hits = ok(gmail.search("in:inbox", 5).await, "search");
    assert!(hits.len() <= 5);
    let found = ok(gmail.envelopes(&hits).await, "envelopes");
    assert!(found.envelopes.len() <= hits.len());
}

#[test]
fn the_out_of_order_spread_is_the_largest_rise() {
    assert_eq!(out_of_order_hours(&[]), 0.0);
    assert_eq!(out_of_order_hours(&[9, 5, 1]), 0.0);
    let hour = 3_600_000;
    assert_eq!(
        out_of_order_hours(&[10 * hour, 2 * hour, 5 * hour, 3 * hour]),
        3.0
    );
}
