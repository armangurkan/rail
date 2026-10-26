//! `snooze.set` and `snooze.clear` through a real daemon over the fake Gmail: the archive goes
//! through the outbox and the snooze becomes active with the message it moved; a replayed token
//! answers with its archive row; a time too soon and a thread with nothing in the inbox are
//! refused; a clear sends the return, then the unread, and the thread is back; and a reply wakes
//! a snooze at once.

use std::time::Duration;

use super::action_tests::{BOUND, Daemon, WORK, daemon};
use crate::auth::unix_now;
use crate::ops::op::{SnoozeClear, SnoozeSet, SyncNow};
use crate::ops::{
    Action, ErrorCode, OpError, SnoozeClearRequest, SnoozeSetRequest, SnoozeSetResponse,
    SnoozeWoke, SyncNowRequest, names,
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
