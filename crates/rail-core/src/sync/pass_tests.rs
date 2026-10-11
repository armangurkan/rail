//! Pass order and error handling found by the final review: new mail during a long grown-window
//! backfill, and failures that must not be swallowed.

use super::*;
use crate::sync::BACKFILL_PAGES_PER_PASS;
use crate::test_support::logged;

/// Review F1: with a cursor and a grown window, each pass catches up new mail before it lists
/// at most `BACKFILL_PAGES_PER_PASS` pages, so a message added mid-backfill appears after the
/// next pass, before the backfill completes.
#[tokio::test]
async fn new_mail_appears_during_a_long_grown_window_backfill() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 1);
    seed_days(&mock, &[("recent", 1)]);
    let old = 2 * BACKFILL_PAGES_PER_PASS + 5;
    for n in 0..old {
        let days = 20 + i64::try_from(n).unwrap();
        mock.seed(envelope(&format!("old{n:03}"), "t-old", ago(days, 0)));
    }
    engine(&h, &mock, 10).pass().await.unwrap();
    assert_eq!(stored(&h), ["recent"]);
    let served = mock.with(|m| m.pages_served);
    mock.with(|m| {
        m.arrive_after_page = Some((served + 3, envelope("newcomer", "t-new", ago(0, 0))));
    });
    let grown = engine(&h, &mock, 0);
    grown.pass().await.unwrap();
    assert!(
        state(&h).backfill_before.is_some(),
        "one pass lists at most {BACKFILL_PAGES_PER_PASS} pages"
    );
    let pages = mock.with(|m| m.pages_served) - served;
    assert_eq!(pages, BACKFILL_PAGES_PER_PASS);
    assert!(!stored(&h).contains(&"newcomer".to_string()));
    let calls = mock.calls();
    let first_backfill = calls.iter().rposition(|c| *c == Call::Changes).unwrap();
    assert_eq!(
        calls[first_backfill + 1],
        Call::Backfill,
        "the delta runs before the backfill"
    );
    grown.pass().await.unwrap();
    assert!(
        stored(&h).contains(&"newcomer".to_string()),
        "the next pass's delta brings it"
    );
    assert!(state(&h).backfill_before.is_some(), "the backfill goes on");
    for _ in 0..3 {
        grown.pass().await.unwrap();
    }
    assert_eq!(state(&h).backfill_before, None);
    assert_eq!(stored(&h).len(), old + 2);
}

/// The initial full backfill (no cursor yet) still lists to the end in one pass.
#[tokio::test]
async fn the_first_full_backfill_is_not_capped() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 1);
    let count = BACKFILL_PAGES_PER_PASS + 5;
    for n in 0..count {
        let days = 1 + i64::try_from(n).unwrap();
        mock.seed(envelope(&format!("m{n:03}"), "t", ago(days, 0)));
    }
    engine(&h, &mock, 0).pass().await.unwrap();
    assert_eq!(stored(&h).len(), count);
    assert!(state(&h).cursor.is_some());
}

/// Review B2: a login failure in a body chunk wins over any other failure in it, and the rest
/// are logged by code.
#[test]
fn a_login_failure_wins_over_other_body_failures_in_the_chunk() {
    let (error, log) = logged(async {
        let h = harness(&["a@example.com"]);
        let mock = MockProvider::new();
        seed_days(&mock, &[("m1", 1), ("m2", 2), ("m3", 3)]);
        mock.fail_next(Call::Body, ProviderError::transient("slow down"));
        mock.fail_next(Call::Body, ProviderError::NeedsAuth { detail: None });
        mock.with(|m| m.body_delay = Duration::from_millis(5));
        engine(&h, &mock, 0).pass().await.unwrap_err()
    });
    assert!(error.needs_auth(), "{error:?}");
    let line = log
        .lines()
        .find(|l| l.contains("more body fetches failed"))
        .unwrap_or_else(|| panic!("{log}"));
    assert!(line.contains("codes=transient"), "{line}");
    assert!(
        !line.contains("slow down"),
        "codes only, never detail: {line}"
    );
}

/// Review B2: a failed counts read after a pass is logged with Rail's words.
#[test]
fn a_failed_counts_read_is_logged() {
    let ((), log) = logged(async {
        let h = harness(&["a@example.com"]);
        let mock = MockProvider::new();
        let engine = engine(&h, &mock, 0);
        Store::open(&h.db)
            .unwrap()
            .conn()
            .execute_batch("DROP TABLE threads")
            .unwrap();
        engine.record(Ok(())).await;
    });
    assert!(
        log.lines()
            .any(|l| l.contains("could not read the account's counts") && l.contains("WARN")),
        "{log}"
    );
}
