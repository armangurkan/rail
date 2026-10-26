//! `snooze.set` and `snooze.clear` through a real daemon over the fake Gmail: the archive goes
//! through the outbox and the snooze becomes active with the message it moved; a replayed token
//! answers with its archive row; a time too soon, and a request with nothing in the inbox, are
//! refused, while a mixed one snoozes the threads it can; a clear sends the return, then the
//! unread, and the thread is back; and a reply wakes a snooze at once.

use std::time::Duration;

use super::action_tests::{BOUND, Daemon, WORK, daemon};
use crate::auth::unix_now;
use crate::ops::op::{Search, SnoozeClear, SnoozeSet, SyncNow};
use crate::ops::{
    Action, ErrorCode, OpError, SearchRequest, SnoozeClearRequest, SnoozeSetRequest,
    SnoozeSetResponse, SnoozeWoke, SyncNowRequest, names,
};
use crate::provider::Change;
use crate::store::Store;
use crate::store::snoozes::SnoozeState;

async fn snooze(
    d: &Daemon,
    threads: Vec<i64>,
    until: i64,
    grace: bool,
    token: &str,
) -> Result<SnoozeSetResponse, OpError> {
    let request = SnoozeSetRequest {
        account: "work".into(),
        threads,
        until,
        grace,
        client_token: token.into(),
    };
    d.client.call::<SnoozeSet>(&request).await
}

/// The snooze of thread `remote`: its state and recorded messages.
fn snooze_of(d: &Daemon, remote: &str) -> Option<(SnoozeState, Vec<String>)> {
    let store = Store::open_reader(&d.paths.db_file()).unwrap();
    store
        .snoozes(1)
        .unwrap()
        .into_iter()
        .find(|s| s.thread_remote == remote)
        .map(|s| (s.state, s.messages))
}

/// Wait (at most `BOUND`) until `check` has something.
async fn until<T>(what: &str, mut check: impl FnMut() -> Option<T>) -> T {
    tokio::time::timeout(BOUND, async {
        loop {
            if let Some(found) = check() {
                return found;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("{what}"))
}

fn sent(d: &Daemon, change: &Change) -> bool {
    d.mock.with(|m| m.applied.iter().any(|(_, c)| c == change))
}

#[tokio::test]
async fn a_snooze_archives_the_thread_through_the_outbox_and_records_what_it_moved() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let answer = snooze(&d, vec![t1], unix_now() + 3_600, false, "s1")
        .await
        .unwrap();
    assert_eq!((answer.rows.len(), answer.snoozed), (1, 1));
    let recorded = until("the archive sent", || match snooze_of(&d, "t1") {
        Some((SnoozeState::Active, messages)) => Some(messages),
        _ => None,
    })
    .await;
    assert_eq!(recorded, vec!["m1".to_string()]);
    assert!(sent(&d, &Change::Archive));
    assert!(!d.in_inbox("m1"));
}

#[tokio::test]
async fn a_replayed_snooze_answers_with_its_first_archive_row() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let at = unix_now() + 3_600;
    let first = snooze(&d, vec![t1], at, true, "s1").await.unwrap();
    let again = snooze(&d, vec![t1], at, true, "s1").await.unwrap();
    assert_eq!(again.rows, first.rows);
}

#[tokio::test]
async fn a_snooze_too_soon_or_of_a_thread_with_nothing_in_the_inbox_is_refused() {
    let d = daemon(WORK).await;
    let (t1, t2) = (d.thread("t1"), d.thread("t2"));
    let soon = snooze(&d, vec![t1], unix_now() + 5, true, "s1")
        .await
        .unwrap_err();
    assert_eq!(soon.code, ErrorCode::InvalidInput);
    d.apply(vec![t2], Action::Archive {}, false, "a2")
        .await
        .unwrap();
    until("t2 archived", || (!d.in_inbox("m2")).then_some(())).await;
    let archived = snooze(&d, vec![t2], unix_now() + 3_600, true, "s2")
        .await
        .unwrap_err();
    assert_eq!(archived.code, ErrorCode::InvalidInput);
    assert_eq!(
        archived.message,
        "Only a thread in the inbox can be snoozed."
    );
}

#[tokio::test]
async fn a_clear_sends_the_return_then_the_unread_and_the_thread_is_back() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    snooze(&d, vec![t1], unix_now() + 3_600, false, "s1")
        .await
        .unwrap();
    until("active", || {
        matches!(snooze_of(&d, "t1"), Some((SnoozeState::Active, _))).then_some(())
    })
    .await;
    let clear = SnoozeClearRequest {
        account: "work".into(),
        threads: vec![t1],
    };
    let answer = d.client.call::<SnoozeClear>(&clear).await.unwrap();
    assert_eq!(answer.returned, 1);
    let back = Change::Unsnooze {
        messages: vec!["m1".into()],
    };
    until("the return sent", || sent(&d, &back).then_some(())).await;
    until("the unread sent", || {
        sent(&d, &Change::Read { on: false }).then_some(())
    })
    .await;
    assert!(d.in_inbox("m1"));
    assert!(matches!(snooze_of(&d, "t1"), Some((SnoozeState::Woken, _))));
}

#[tokio::test]
async fn a_watcher_is_told_when_a_snooze_puts_threads_back() {
    let d = daemon(WORK).await;
    let (t1, t2) = (d.thread("t1"), d.thread("t2"));
    let mut notes = d.client.notifications();
    let watch = SyncNowRequest {
        account: None,
        watch: true,
    };
    d.client.call::<SyncNow>(&watch).await.unwrap();
    snooze(&d, vec![t1], unix_now() + 3_600, false, "s1")
        .await
        .unwrap();
    until("active", || {
        matches!(snooze_of(&d, "t1"), Some((SnoozeState::Active, _))).then_some(())
    })
    .await;
    // Due now, as an incoming reply would make it; then wake the worker: a clear of a thread
    // with no snooze changes nothing but tells the worker to look.
    Store::open(&d.paths.db_file())
        .unwrap()
        .conn()
        .execute("UPDATE snoozes SET until = ?1", [unix_now() - 1])
        .unwrap();
    let nudge = SnoozeClearRequest {
        account: "work".into(),
        threads: vec![t2],
    };
    d.client.call::<SnoozeClear>(&nudge).await.unwrap();
    let woke = tokio::time::timeout(BOUND, async {
        loop {
            let note = notes.recv().await.unwrap();
            if note.method == names::SNOOZE_WOKE {
                return note.params;
            }
        }
    })
    .await
    .unwrap();
    let woke: SnoozeWoke = serde_json::from_value(woke).unwrap();
    assert_eq!(
        woke,
        SnoozeWoke {
            account: "work".into(),
            threads: vec![t1],
        }
    );
}

#[tokio::test]
async fn a_reply_to_a_snoozed_thread_brings_it_back_without_waiting_for_the_worker() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let mut notes = d.client.notifications();
    let watch = SyncNowRequest {
        account: None,
        watch: true,
    };
    d.client.call::<SyncNow>(&watch).await.unwrap();
    snooze(&d, vec![t1], unix_now() + 3_600, false, "s1")
        .await
        .unwrap();
    until("active", || {
        matches!(snooze_of(&d, "t1"), Some((SnoozeState::Active, _))).then_some(())
    })
    .await;
    // The worker now sleeps toward the snooze's hour; the reply must not wait for it.
    d.mock.add(crate::provider::mock::envelope(
        "reply",
        "t1",
        unix_now() * 1_000,
    ));
    let sync = SyncNowRequest {
        account: None,
        watch: false,
    };
    d.client.call::<SyncNow>(&sync).await.unwrap();
    let woke = tokio::time::timeout(BOUND, async {
        loop {
            let note = notes.recv().await.unwrap();
            if note.method == names::SNOOZE_WOKE {
                return note.params;
            }
        }
    })
    .await
    .expect("snooze.woke within the bound");
    let woke: SnoozeWoke = serde_json::from_value(woke).unwrap();
    assert_eq!(woke.threads, [t1]);
}

#[tokio::test]
async fn a_reply_a_search_brings_in_wakes_a_snooze_without_waiting_for_the_worker() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let mut notes = d.client.notifications();
    let watch = SyncNowRequest {
        account: None,
        watch: true,
    };
    d.client.call::<SyncNow>(&watch).await.unwrap();
    snooze(&d, vec![t1], unix_now() + 3_600, false, "s1")
        .await
        .unwrap();
    until("active", || {
        matches!(snooze_of(&d, "t1"), Some((SnoozeState::Active, _))).then_some(())
    })
    .await;
    // Seeded, so no sync's history brings the reply: only the search does.
    d.mock.seed(crate::provider::mock::envelope(
        "reply",
        "t1",
        unix_now() * 1_000,
    ));
    d.mock.with(|m| m.search_hits = vec!["reply".into()]);
    let request = SearchRequest {
        query: "reply".into(),
        account: None,
        limit: 10,
    };
    d.client.call::<Search>(&request).await.unwrap();
    let woke = tokio::time::timeout(BOUND, async {
        loop {
            let note = notes.recv().await.unwrap();
            if note.method == names::SNOOZE_WOKE {
                return note.params;
            }
        }
    })
    .await
    .expect("snooze.woke within the bound");
    let woke: SnoozeWoke = serde_json::from_value(woke).unwrap();
    assert_eq!(woke.threads, [t1]);
}

#[tokio::test]
async fn a_thread_the_account_does_not_hold_is_refused() {
    let d = daemon(WORK).await;
    let refused = snooze(&d, vec![999_999], unix_now() + 3_600, false, "s1")
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);
    assert!(snooze_of(&d, "t1").is_none());
}

#[tokio::test]
async fn snoozing_an_archived_snoozed_thread_again_takes_the_new_time() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    snooze(&d, vec![t1], unix_now() + 3_600, false, "s1")
        .await
        .unwrap();
    until("active", || {
        matches!(snooze_of(&d, "t1"), Some((SnoozeState::Active, _))).then_some(())
    })
    .await;
    let later = unix_now() + 7_200;
    let again = snooze(&d, vec![t1], later, false, "s2").await.unwrap();
    assert_eq!((again.rows, again.snoozed), (vec![], 1));
    let store = Store::open_reader(&d.paths.db_file()).unwrap();
    assert_eq!(store.snoozes(1).unwrap()[0].until, later);
}

/// Archive `threads` (no grace) and wait until their mail has left the inbox.
async fn archived(d: &Daemon, threads: &[(&str, &str)]) {
    let ids = threads.iter().map(|(t, _)| d.thread(t)).collect();
    d.apply(ids, Action::Archive {}, false, "archive")
        .await
        .unwrap();
    for (_, m) in threads {
        until("archived", || (!d.in_inbox(m)).then_some(())).await;
    }
}

#[tokio::test]
async fn a_mixed_snooze_takes_the_inbox_thread_and_leaves_the_archived_one() {
    let d = daemon(WORK).await;
    let (t1, t2) = (d.thread("t1"), d.thread("t2"));
    archived(&d, &[("t2", "m2")]).await;
    let answer = snooze(&d, vec![t1, t2], unix_now() + 3_600, false, "s1")
        .await
        .unwrap();
    assert_eq!((answer.rows.len(), answer.snoozed), (1, 1));
    until("t1 snoozed", || {
        matches!(snooze_of(&d, "t1"), Some((SnoozeState::Active, _))).then_some(())
    })
    .await;
    assert_eq!(snooze_of(&d, "t2"), None, "the archived thread is left out");
}

#[tokio::test]
async fn a_snooze_with_nothing_in_the_inbox_among_its_threads_is_refused() {
    let d = daemon(WORK).await;
    let (t1, t2) = (d.thread("t1"), d.thread("t2"));
    archived(&d, &[("t1", "m1"), ("t2", "m2")]).await;
    let refused = snooze(&d, vec![t1, t2], unix_now() + 3_600, false, "s1")
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::InvalidInput);
    assert_eq!(
        refused.message,
        "Only a thread in the inbox can be snoozed."
    );
    assert!(snooze_of(&d, "t1").is_none() && snooze_of(&d, "t2").is_none());
}

#[tokio::test]
async fn a_snoozed_thread_and_a_fresh_one_are_snoozed_together() {
    let d = daemon(WORK).await;
    let (t1, t2) = (d.thread("t1"), d.thread("t2"));
    snooze(&d, vec![t1], unix_now() + 3_600, false, "s1")
        .await
        .unwrap();
    until("t1 active", || {
        matches!(snooze_of(&d, "t1"), Some((SnoozeState::Active, _))).then_some(())
    })
    .await;
    let later = unix_now() + 7_200;
    let both = snooze(&d, vec![t1, t2], later, false, "s2").await.unwrap();
    assert_eq!((both.rows.len(), both.snoozed), (1, 2), "one archive: t2's");
    let store = Store::open_reader(&d.paths.db_file()).unwrap();
    let mut ends: Vec<i64> = store.snoozes(1).unwrap().iter().map(|s| s.until).collect();
    ends.dedup();
    assert_eq!(ends, [later]);
}
