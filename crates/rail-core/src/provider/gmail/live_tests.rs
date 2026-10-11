//! The one live test: read-only calls against a real Gmail account, run by hand with
//! `RAIL_GMAIL_LIVE_ACCOUNT=<name> cargo test -p rail-core -- --ignored gmail_live`.
//! It uses your real config and token store (the `RAIL_*_DIR` variables apply) and only
//! ever sends GET requests, so it never changes the mailbox. It confirms the Gmail shapes
//! this phase assumed:
//!
//! - `after:` and `before:` take epoch seconds, and listings come newest first;
//! - a listing with no match answers without a `messages` key and Rail reads it as empty;
//! - subjects hold no raw encoded words and snippets no `&#39;`-style references;
//! - bodies (base64url, with or without padding) decode into text;
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
use crate::provider::{Envelope, FolderKind, Provider, ProviderError, SyncCursor, Window};
use crate::sync::manager::ProviderRequest;
use crate::sync::providers::gmail_for;

/// How many of the listed messages have their bodies fetched.
const BODIES_CHECKED: usize = 5;

fn live_provider() -> Arc<dyn Provider> {
    let who = std::env::var("RAIL_GMAIL_LIVE_ACCOUNT")
        .expect("set RAIL_GMAIL_LIVE_ACCOUNT to an account name");
    let paths = Paths::from_env().unwrap();
    let config = config::load(&paths.config_file()).unwrap();
    let account = crate::accounts::find_account(&config, &who)
        .unwrap()
        .clone();
    gmail_for(ProviderRequest {
        paths,
        config,
        account,
        ids: ItemIdCache::default(),
        token_url: None,
        gmail_url: None,
        observer: Arc::new(|wait| eprintln!("retrying in {wait:?}")),
    })
    .unwrap()
}

async fn check_labels_and_profile(gmail: &dyn Provider) -> SyncCursor {
    let folders = gmail.folders().await.unwrap();
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
    let profile = gmail.profile().await.unwrap();
    assert!(
        !profile.cursor.0.is_empty() && profile.cursor.0.bytes().all(|b| b.is_ascii_digit()),
        "historyId is numeric: {:?}",
        profile.cursor
    );
    profile.cursor
}

/// Lists the last 30 days (ending a day ago, so both `after:` and `before:` are used) and
/// checks the window and the order.
async fn check_window_and_order(gmail: &dyn Provider) -> Vec<Envelope> {
    let now = unix_now();
    let window = Window {
        after_ms: Some((now - 30 * DAY_SECS) * 1000),
        before_ms: Some((now - DAY_SECS) * 1000),
    };
    let page = gmail.backfill(&window, None).await.unwrap();
    let dates: Vec<i64> = page.envelopes.iter().map(|e| e.internal_date).collect();
    assert!(
        dates.windows(2).all(|w| w[0] >= w[1]),
        "listed newest first: {dates:?}"
    );
    assert!(
        dates.iter().all(|d| window.contains(*d)),
        "epoch-second after:/before: bound the listing: {dates:?}"
    );
    if dates.is_empty() {
        eprintln!("NOT CONFIRMED: no mail 1 to 30 days old; window, order and text checks skipped");
    }
    page.envelopes
}

/// A window in the future lists nothing: the answer has no `messages` key.
async fn check_empty_listing(gmail: &dyn Provider) {
    let future = Window {
        after_ms: Some((unix_now() + DAY_SECS) * 1000),
        before_ms: None,
    };
    let page = gmail.backfill(&future, None).await.unwrap();
    assert!(
        page.envelopes.is_empty() && page.next.is_none(),
        "empty list reads as empty"
    );
    let none = gmail
        .search("rail-live-test-no-such-word-0f3a9c71", 5)
        .await
        .unwrap();
    assert!(
        none.is_empty(),
        "a search with no match reads as empty: {none:?}"
    );
}

async fn check_text(gmail: &dyn Provider, envelopes: &[Envelope]) {
    let mut encoded_seen = 0;
    let mut entity_seen = 0;
    let mut bodies = 0;
    for envelope in envelopes {
        assert!(
            !envelope.subject.contains("=?"),
            "encoded words decoded: {:?}",
            envelope.subject
        );
        assert!(
            !envelope.snippet.contains("&#39;"),
            "snippet references decoded: {:?}",
            envelope.snippet
        );
        encoded_seen += usize::from(!envelope.subject.is_ascii());
        entity_seen += usize::from(envelope.snippet.contains('\''));
    }
    for envelope in envelopes.iter().take(BODIES_CHECKED) {
        let body = gmail.body(&envelope.remote_id).await.unwrap();
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
    let changes = gmail.changes(current).await.unwrap();
    assert!(!changes.cursor.0.is_empty());
    let expired = gmail.changes(&SyncCursor("1".into())).await;
    eprintln!("a very old historyId gives: {expired:?} (expected CursorExpired)");
    assert!(matches!(expired, Err(ProviderError::CursorExpired)));
}

#[tokio::test]
#[ignore = "talks to a real Gmail account; run by hand"]
async fn gmail_live_read_only() {
    let gmail = live_provider();
    let cursor = check_labels_and_profile(gmail.as_ref()).await;
    let envelopes = check_window_and_order(gmail.as_ref()).await;
    check_empty_listing(gmail.as_ref()).await;
    check_text(gmail.as_ref(), &envelopes).await;
    check_history(gmail.as_ref(), &cursor).await;
    let hits = gmail.search("in:inbox", 5).await.unwrap();
    assert!(hits.len() <= 5);
    let found = gmail.envelopes(&hits).await.unwrap();
    assert!(found.envelopes.len() <= hits.len());
}
