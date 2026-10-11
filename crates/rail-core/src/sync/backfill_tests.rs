//! Gmail lists only roughly newest first and silently answers a
//! page token it does not know. A backfill goes on with Gmail's page token, lists the window
//! from the top when it cannot, never fetches metadata it already stored, never bounds a
//! listing by its date marker, and never stalls on pages that store nothing.

use super::*;
use crate::sync::{
    BACKFILL_MAX_PAGES_PER_PASS, BACKFILL_PAGES_PER_PASS, BACKFILL_TOKEN_MAX_AGE_SECS,
};

const HOUR_MS: i64 = 3_600_000;

/// A mailbox listed out of order by up to 3 hours (far more than one second), one message a
/// page: "recent", `old` messages an hour apart from 20 days back, and "imported", years old
/// but listed first (as Gmail lists imported mail), which drags the oldest-seen date back
/// years on the very first page. `messages(old)` messages in all.
fn shuffled(old: usize) -> MockProvider {
    let mock = MockProvider::new();
    mock.with(|m| {
        m.page_size = 1;
        m.shuffle_within_ms = 3 * HOUR_MS;
        m.list_first = vec!["imported".into()];
    });
    seed_days(&mock, &[("recent", 1), ("imported", 3_000)]);
    for n in 0..old {
        let hours = i64::try_from(n).unwrap() * HOUR_MS;
        mock.seed(envelope(&format!("old{n:03}"), "t-old", ago(20, 0) - hours));
    }
    mock
}

fn messages(old: usize) -> usize {
    old + 2
}

/// Passes until the backfill is done (at most `limit`); how many it took.
async fn until_backfilled(h: &Harness, engine: &Engine, limit: usize) -> usize {
    for n in 1..=limit {
        engine.pass().await.unwrap();
        let s = state(h);
        if s.backfill_before.is_none() && s.backfill_resume.is_none() {
            return n;
        }
    }
    panic!("the backfill did not finish in {limit} passes");
}

/// The listing is out of order by hours and an imported message years old comes first; a
/// grown window capped per pass still stores every message once it completes, and fetches no
/// metadata twice.
#[tokio::test]
async fn an_out_of_order_listing_loses_nothing_across_capped_passes() {
    let h = harness(&["a@example.com"]);
    let old = 3 * BACKFILL_PAGES_PER_PASS;
    let mock = shuffled(old);
    engine(&h, &mock, 10).pass().await.unwrap();
    let grown = engine(&h, &mock, 0);
    let passes = until_backfilled(&h, &grown, 10).await;
    assert!(passes > 1, "the cap applied ({passes} passes)");
    assert_eq!(stored(&h).len(), messages(old), "every message is stored");
    assert!(mock.metadata_refetches().is_empty());
}

/// The stored token is refused after the imported message dragged the oldest-seen
/// date back years. The backfill lists the window again from the top, with no date bound
/// (a `before: marker + 3 days` bound would skip everything between), stores everything, and
/// asks no stored id's metadata again.
#[tokio::test]
async fn a_refused_token_relists_the_window_from_the_top() {
    let h = harness(&["a@example.com"]);
    let old = 3 * BACKFILL_PAGES_PER_PASS;
    let mock = shuffled(old);
    engine(&h, &mock, 10).pass().await.unwrap();
    let grown = engine(&h, &mock, 0);
    grown.pass().await.unwrap();
    assert_eq!(
        state(&h).backfill_before,
        Some(ago(3_000, 0)),
        "dragged back"
    );
    mock.expire_page_tokens();
    let listed_before = mock.backfill_windows().len();
    until_backfilled(&h, &grown, 10).await;
    let tokens = mock.with(|m| m.list_tokens.clone());
    assert!(
        tokens[listed_before].is_some(),
        "the stored token was tried"
    );
    assert_eq!(
        tokens[listed_before + 1],
        None,
        "then the top of the window"
    );
    assert_eq!(
        mock.backfill_windows()[listed_before + 1],
        Window {
            after_ms: None,
            before_ms: Some(ago(10, 1000)),
        },
        "the grown window's own query, no bound from the marker"
    );
    assert_eq!(stored(&h).len(), messages(old), "every message is stored");
    assert!(
        mock.metadata_refetches().is_empty(),
        "no stored id's metadata is fetched again: {:?}",
        mock.metadata_refetches()
    );
    assert_eq!(mock.with(|m| m.metadata_gets.len()), messages(old));
}

/// As Gmail does, the mock answers a stale token with the first page. Nothing can
/// tell, and nothing needs to: everything ends up stored, each message fetched once.
#[tokio::test]
async fn a_silently_ignored_token_still_ends_with_everything_stored() {
    let h = harness(&["a@example.com"]);
    let old = 3 * BACKFILL_PAGES_PER_PASS;
    let mock = shuffled(old);
    mock.with(|m| m.ignore_unknown_tokens = true);
    engine(&h, &mock, 10).pass().await.unwrap();
    let grown = engine(&h, &mock, 0);
    grown.pass().await.unwrap();
    mock.expire_page_tokens();
    until_backfilled(&h, &grown, 10).await;
    assert_eq!(stored(&h).len(), messages(old));
    assert!(mock.metadata_refetches().is_empty());
}

/// A capped re-list from the top keeps its place across passes: the token is stored
/// after every listed page, pages of stored mail included, so the next pass does not start
/// from the top again.
#[tokio::test]
async fn a_capped_relist_makes_progress_across_passes() {
    let h = harness(&["a@example.com"]);
    let old = 4 * BACKFILL_PAGES_PER_PASS;
    let mock = shuffled(old);
    engine(&h, &mock, 10).pass().await.unwrap();
    let grown = engine(&h, &mock, 0);
    grown.pass().await.unwrap();
    mock.expire_page_tokens();
    grown.pass().await.unwrap();
    let after_relist = state(&h)
        .backfill_resume
        .expect("a token after the re-list");
    let listed_before = mock.backfill_windows().len();
    grown.pass().await.unwrap();
    let tokens = mock.with(|m| m.list_tokens.clone());
    assert_eq!(
        tokens[listed_before].as_ref(),
        Some(&after_relist.token.0),
        "the next pass goes on where the re-list stopped"
    );
    assert!(
        mock.backfill_windows().len() - listed_before <= BACKFILL_PAGES_PER_PASS + 1,
        "no page of stored mail is listed again"
    );
    until_backfilled(&h, &grown, 10).await;
    assert_eq!(stored(&h).len(), messages(old));
}

/// Gmail is not known to refuse a stale token, so any permanent failure of a
/// listing that carried one lists from the top once, rather than failing every pass.
#[tokio::test]
async fn a_failure_with_a_stored_token_relists_once_instead_of_failing_every_pass() {
    let h = harness(&["a@example.com"]);
    let mock = shuffled(10);
    mock.fail_after(
        Call::Backfill,
        4,
        ProviderError::transient("the network went away"),
    );
    assert!(engine(&h, &mock, 0).pass().await.is_err());
    assert!(state(&h).backfill_resume.is_some());
    mock.fail_next(Call::Backfill, ProviderError::permanent("bad request"));
    engine(&h, &mock, 0).pass().await.unwrap();
    let tokens = mock.with(|m| m.list_tokens.clone());
    assert!(tokens[5].is_some() && tokens[6].is_none(), "{tokens:?}");
    assert_eq!(stored(&h).len(), messages(10));
    assert!(mock.metadata_refetches().is_empty());
    mock.fail_next(Call::Backfill, ProviderError::permanent("down"));
    mock.with(|m| m.expired_below = u64::MAX);
    assert!(
        engine(&h, &mock, 0).pass().await.is_err(),
        "without a token, a failure still ends the pass"
    );
}

/// A token older than `BACKFILL_TOKEN_MAX_AGE_SECS` (the daemon was off for a week) is not
/// sent: the backfill lists the window from the top, and still stores everything exactly once.
#[tokio::test]
async fn an_old_token_is_not_used_after_a_long_stop() {
    let h = harness(&["a@example.com"]);
    let mock = shuffled(30);
    mock.fail_after(
        Call::Backfill,
        10,
        ProviderError::transient("the network went away"),
    );
    assert!(engine(&h, &mock, 0).pass().await.is_err());
    assert!(state(&h).backfill_resume.is_some());
    let later = NOW + BACKFILL_TOKEN_MAX_AGE_SECS + 1;
    let restarted = engine_at(&h, &mock, "a@example.com", settings(0), later);
    restarted.pass().await.unwrap();
    let tokens = mock.with(|m| m.list_tokens.clone());
    assert_eq!(tokens[11], None, "the old token is not sent");
    assert_eq!(mock.backfill_windows()[11], Window::default(), "the top");
    assert_eq!(stored(&h).len(), messages(30));
    assert!(mock.metadata_refetches().is_empty());
    assert!(state(&h).cursor.is_some());
}

/// A window grown while a backfill was under way: the stored token's query no longer reaches
/// back far enough, so it is dropped and the grown window is listed from the top.
#[tokio::test]
async fn a_window_grown_mid_backfill_drops_the_token() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 1);
    seed_days(&mock, &[("d1", 1), ("d2", 2), ("d3", 3), ("d20", 20)]);
    mock.fail_after(
        Call::Backfill,
        1,
        ProviderError::transient("the network went away"),
    );
    assert!(engine(&h, &mock, 10).pass().await.is_err());
    let resume = state(&h).backfill_resume.expect("a token");
    assert_eq!(resume.listing.after_ms, Some(ago(10, 0)));
    engine(&h, &mock, 30).pass().await.unwrap();
    let tokens = mock.with(|m| m.list_tokens.clone());
    assert_eq!(tokens[2], None, "the token is dropped");
    assert_eq!(mock.backfill_windows()[2].after_ms, Some(ago(30, 0)));
    assert_eq!(stored(&h), ["d1", "d2", "d20", "d3"]);
    assert!(mock.metadata_refetches().is_empty());
}

/// An expired cursor mid grown-window backfill: the resync clears the page token (and the
/// marker) and backfills in full from the top.
#[tokio::test]
async fn a_resync_clears_the_page_token() {
    let h = harness(&["a@example.com"]);
    let old = 2 * BACKFILL_PAGES_PER_PASS;
    let mock = shuffled(old);
    engine(&h, &mock, 10).pass().await.unwrap();
    let grown = engine(&h, &mock, 0);
    grown.pass().await.unwrap();
    assert!(state(&h).backfill_resume.is_some());
    mock.with(|m| m.expired_below = u64::MAX);
    let listed_before = mock.backfill_windows().len();
    let later = engine_at(&h, &mock, "a@example.com", settings(0), NOW + 60);
    mock.with(|m| m.expired_below = 0);
    mock.fail_next(Call::Changes, ProviderError::CursorExpired);
    later.pass().await.unwrap();
    let tokens = mock.with(|m| m.list_tokens.clone());
    assert_eq!(tokens[listed_before], None, "the resync lists from the top");
    assert_eq!(mock.backfill_windows()[listed_before], Window::default());
    let after = state(&h);
    assert_eq!((after.backfill_resume, after.backfill_before), (None, None));
    assert_eq!(stored(&h).len(), messages(old));
}

/// Review focus 1 again, through the fallback: messages sharing the oldest second survive a
/// refused token too.
#[tokio::test]
async fn messages_sharing_the_oldest_second_survive_a_refused_token() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 2);
    for (id, extra) in [("m1", 0), ("m2", 100), ("m3", 200), ("m4", 300)] {
        mock.seed(envelope(id, "t", ago(3, extra)));
    }
    mock.fail_after(
        Call::Backfill,
        1,
        ProviderError::transient("the network went away"),
    );
    let engine = engine(&h, &mock, 0);
    assert!(engine.pass().await.is_err());
    mock.expire_page_tokens();
    engine.pass().await.unwrap();
    assert_eq!(stored(&h), ["m1", "m2", "m3", "m4"]);
    assert!(mock.metadata_refetches().is_empty());
}

/// The parked final residual: more than `BACKFILL_PAGES_PER_PASS` consecutive pages of
/// unreadable mail no longer stall a grown-window backfill. Pages without progress do not
/// count; they go on by token within the pass.
#[tokio::test]
async fn pages_of_unreadable_mail_do_not_stall_a_capped_backfill() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 1);
    seed_days(&mock, &[("recent", 1)]);
    let broken = BACKFILL_PAGES_PER_PASS + 10;
    for n in 0..broken {
        let id = format!("bad{n:03}");
        mock.seed(envelope(
            &id,
            "t-bad",
            ago(20, 0) - i64::try_from(n).unwrap() * HOUR_MS,
        ));
        mock.with(|m| m.broken.insert(id));
    }
    seed_days(&mock, &[("good1", 40), ("good2", 41)]);
    engine(&h, &mock, 10).pass().await.unwrap();
    let served = mock.with(|m| m.pages_served);
    let grown = engine(&h, &mock, 0);
    assert_eq!(until_backfilled(&h, &grown, 1).await, 1, "one pass");
    assert_eq!(mock.with(|m| m.pages_served) - served, broken + 2);
    assert_eq!(stored(&h), ["good1", "good2", "recent"]);
    assert_eq!(state(&h).backfill_window, Some(0), "the window is promoted");
}

/// However pathological the listing, one pass asks for at most `BACKFILL_MAX_PAGES_PER_PASS`
/// pages without progress; the next pass goes on by token.
#[tokio::test]
async fn a_pass_lists_at_most_the_hard_ceiling_of_pages() {
    for history_days in [10, 0] {
        let h = harness(&["a@example.com"]);
        let mock = MockProvider::new();
        mock.with(|m| m.page_size = 1);
        seed_days(&mock, &[("recent", 1)]);
        for n in 0..BACKFILL_MAX_PAGES_PER_PASS + 20 {
            let id = format!("bad{n:03}");
            mock.seed(envelope(
                &id,
                "t-bad",
                ago(20, 0) - i64::try_from(n).unwrap() * HOUR_MS,
            ));
            mock.with(|m| m.broken.insert(id));
        }
        seed_days(&mock, &[("good", 400)]);
        // history_days 10 first: the next engine's whole-mailbox window is a grown one.
        // history_days 0 alone: the first full backfill.
        engine(&h, &mock, history_days).pass().await.unwrap();
        let served = mock.with(|m| m.pages_served);
        let whole = engine(&h, &mock, 0);
        whole.pass().await.unwrap();
        let first = mock.with(|m| m.pages_served) - served;
        if history_days == 0 {
            assert_eq!(
                served,
                BACKFILL_MAX_PAGES_PER_PASS + 1,
                "full: idle pages capped"
            );
        } else {
            assert_eq!(
                first, BACKFILL_MAX_PAGES_PER_PASS,
                "grown: all pages capped"
            );
        }
        until_backfilled(&h, &whole, 3).await;
        assert_eq!(
            stored(&h),
            ["good", "recent"],
            "history_days {history_days}"
        );
        assert!(state(&h).cursor.is_some());
    }
}

/// If Gmail answered every resumed token with the first page, each pass would
/// re-walk the stored prefix of the listing. With the ceiling fixed at
/// `BACKFILL_MAX_PAGES_PER_PASS`, a prefix longer than that was a livelock (nothing lost,
/// never done); the ceiling now grows with what is stored, so the backfill completes.
#[tokio::test]
async fn a_backfill_completes_when_every_resumed_token_restarts_the_listing() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| {
        m.page_size = 1;
        m.ignore_unknown_tokens = true;
    });
    seed_days(&mock, &[("recent", 1)]);
    let old = BACKFILL_MAX_PAGES_PER_PASS + 2 * BACKFILL_PAGES_PER_PASS;
    for n in 0..old {
        let hours = i64::try_from(n).unwrap() * HOUR_MS;
        mock.seed(envelope(&format!("old{n:03}"), "t-old", ago(20, 0) - hours));
    }
    engine(&h, &mock, 10).pass().await.unwrap();
    let grown = engine(&h, &mock, 0);
    let bound = old / BACKFILL_PAGES_PER_PASS + 2;
    let mut passes = 0;
    while passes < bound {
        mock.expire_page_tokens();
        grown.pass().await.unwrap();
        passes += 1;
        let s = state(&h);
        if s.backfill_before.is_none() && s.backfill_resume.is_none() {
            break;
        }
    }
    assert_eq!(
        state(&h).backfill_window,
        Some(0),
        "done in {passes} passes"
    );
    assert_eq!(stored(&h).len(), old + 1);
    assert!(mock.metadata_refetches().is_empty());
}
