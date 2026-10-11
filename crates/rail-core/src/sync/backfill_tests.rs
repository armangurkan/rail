//! Ruling P4-LIVE: Gmail lists only roughly newest first. A backfill goes on with Gmail's page
//! token, falls back to its date marker with an overlap when it cannot, never fetches metadata
//! it already stored, and never stalls on pages that store nothing.

use super::*;
use crate::sync::{
    BACKFILL_MAX_PAGES_PER_PASS, BACKFILL_OVERLAP_MS, BACKFILL_PAGES_PER_PASS,
    BACKFILL_TOKEN_MAX_AGE_SECS,
};

const HOUR_MS: i64 = 3_600_000;

/// A mailbox listed out of order by up to 3 hours (far more than one second), one message a
/// page: "recent" plus `old` messages an hour apart from 20 days back, so a grown window needs
/// more than one capped pass.
fn shuffled(old: usize) -> MockProvider {
    let mock = MockProvider::new();
    mock.with(|m| {
        m.page_size = 1;
        m.shuffle_within_ms = 3 * HOUR_MS;
    });
    seed_days(&mock, &[("recent", 1)]);
    for n in 0..old {
        let hours = i64::try_from(n).unwrap() * HOUR_MS;
        mock.seed(envelope(&format!("old{n:03}"), "t-old", ago(20, 0) - hours));
    }
    mock
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

/// The listing is out of order by hours; a grown window capped per pass still stores every
/// message once it completes, and fetches no metadata twice. (Failed before P4-LIVE: 59 of 61.)
#[tokio::test]
async fn an_out_of_order_listing_loses_nothing_across_capped_passes() {
    let h = harness(&["a@example.com"]);
    let old = 3 * BACKFILL_PAGES_PER_PASS;
    let mock = shuffled(old);
    engine(&h, &mock, 10).pass().await.unwrap();
    let grown = engine(&h, &mock, 0);
    let passes = until_backfilled(&h, &grown, 10).await;
    assert!(passes > 1, "the cap applied ({passes} passes)");
    assert_eq!(stored(&h).len(), old + 1, "every message is stored");
    assert!(mock.metadata_refetches().is_empty());
}

/// Gmail refuses the stored token (stale): the backfill lists again below its marker with
/// `BACKFILL_OVERLAP_MS` above it, stores everything, and asks no stored id's metadata again.
#[tokio::test]
async fn a_refused_token_falls_back_to_the_marker_with_an_overlap() {
    let h = harness(&["a@example.com"]);
    let old = 3 * BACKFILL_PAGES_PER_PASS;
    let mock = shuffled(old);
    engine(&h, &mock, 10).pass().await.unwrap();
    let grown = engine(&h, &mock, 0);
    grown.pass().await.unwrap();
    let marker = state(&h)
        .backfill_before
        .expect("a marker after one capped pass");
    assert!(state(&h).backfill_resume.is_some());
    mock.expire_page_tokens();
    let listed_before = mock.backfill_windows().len();
    until_backfilled(&h, &grown, 10).await;
    let windows = mock.backfill_windows();
    let tokens = mock.with(|m| m.list_tokens.clone());
    assert!(
        tokens[listed_before].is_some(),
        "the stored token was tried"
    );
    assert_eq!(
        tokens[listed_before + 1],
        None,
        "then a listing without one"
    );
    assert_eq!(
        windows[listed_before + 1].before_ms,
        Some(marker + BACKFILL_OVERLAP_MS),
        "below the marker, with the overlap above it"
    );
    assert_eq!(stored(&h).len(), old + 1, "every message is stored");
    assert!(
        mock.metadata_refetches().is_empty(),
        "no stored id's metadata is fetched again: {:?}",
        mock.metadata_refetches()
    );
    assert_eq!(mock.with(|m| m.metadata_gets.len()), old + 1);
}

/// A token older than `BACKFILL_TOKEN_MAX_AGE_SECS` (the daemon was off) is not sent: the
/// backfill lists from its marker with the overlap, and still stores everything exactly once.
#[tokio::test]
async fn an_old_token_is_not_used_after_a_long_stop() {
    let h = harness(&["a@example.com"]);
    let mock = shuffled(30);
    mock.fail_after(Call::Backfill, 10, ProviderError::permanent("boom"));
    assert!(engine(&h, &mock, 0).pass().await.is_err());
    let marker = state(&h).backfill_before.unwrap();
    assert!(state(&h).backfill_resume.is_some());
    let later = NOW + BACKFILL_TOKEN_MAX_AGE_SECS + 1;
    let restarted = engine_at(&h, &mock, "a@example.com", settings(0), later);
    restarted.pass().await.unwrap();
    let tokens = mock.with(|m| m.list_tokens.clone());
    assert_eq!(tokens[11], None, "the old token is not sent");
    assert_eq!(
        mock.backfill_windows()[11].before_ms,
        Some(marker + BACKFILL_OVERLAP_MS)
    );
    assert_eq!(stored(&h).len(), 31);
    assert!(mock.metadata_refetches().is_empty());
    assert!(state(&h).cursor.is_some());
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
    mock.fail_after(Call::Backfill, 1, ProviderError::permanent("boom"));
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
