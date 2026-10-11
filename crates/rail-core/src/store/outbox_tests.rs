//! The queue: claim order and `not_before`, the client token, transient retries and the cap,
//! undo of claimed rows, recovery, depth, and an enqueue that keeps only the threads the
//! account still holds.

use super::apply_local_tests::{enqueue, seeded, thread};
use super::outbox::{Failure, MAX_ATTEMPTS, OutboxOutcome, OutboxState, OutboxWrite};
use crate::ops::Action;

const NOW: i64 = 1_760_000_000;

fn claim(store: &mut super::Store, at: i64) -> Option<super::outbox::OutboxRow> {
    match store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, at)
        .unwrap()
    {
        OutboxOutcome::Claimed(row) => row,
        other => panic!("{other:?}"),
    }
}

#[test]
fn claim_takes_the_oldest_due_row_and_honours_not_before() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let a = enqueue(&mut store, Action::Star { on: true }, vec![t1], "a");
    let b = enqueue(&mut store, Action::Archive {}, vec![t1], "b");
    assert!(claim(&mut store, NOW + 9).is_none(), "inside the grace");
    let first = claim(&mut store, NOW + 10).unwrap();
    assert_eq!(
        (first.id, first.state, first.thread_ids.clone()),
        (a, OutboxState::Inflight, vec![t1])
    );
    assert_eq!(first.action, Action::Star { on: true });
    assert!(
        claim(&mut store, NOW + 10).is_none(),
        "b waits for a, which shares its thread"
    );
    store
        .outbox_write(&OutboxWrite::Complete { id: a }, NOW + 10)
        .unwrap();
    assert_eq!(claim(&mut store, NOW + 10).unwrap().id, b);
    assert!(claim(&mut store, NOW + 10).is_none());
}

#[test]
fn a_backed_off_row_holds_back_a_later_due_row_on_its_thread() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let a = enqueue(&mut store, Action::Star { on: true }, vec![t1], "a");
    let b = enqueue(&mut store, Action::Star { on: false }, vec![t1], "b");
    claim(&mut store, NOW + 10).unwrap();
    let fail = OutboxWrite::Fail {
        id: a,
        failure: Failure::Transient {
            error: "Gmail is busy".into(),
            retry_at: NOW + 70,
        },
    };
    store.outbox_write(&fail, NOW + 10).unwrap();
    assert!(
        claim(&mut store, NOW + 20).is_none(),
        "b is due but a is older"
    );
    assert_eq!(claim(&mut store, NOW + 70).unwrap().id, a);
    assert!(claim(&mut store, NOW + 70).is_none(), "a is inflight");
    store
        .outbox_write(&OutboxWrite::Complete { id: a }, NOW + 71)
        .unwrap();
    assert_eq!(claim(&mut store, NOW + 71).unwrap().id, b);
}

#[test]
fn next_due_skips_rows_held_back_by_an_older_row_on_their_thread() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let a = enqueue(&mut store, Action::Star { on: true }, vec![t1], "a");
    enqueue(&mut store, Action::Star { on: false }, vec![t1], "b");
    claim(&mut store, NOW + 10).unwrap();
    let fail = OutboxWrite::Fail {
        id: a,
        failure: Failure::Transient {
            error: "Gmail is busy".into(),
            retry_at: NOW + 70,
        },
    };
    store.outbox_write(&fail, NOW + 10).unwrap();
    assert_eq!(
        store.outbox_next_due(1).unwrap(),
        Some(NOW + 70),
        "a's time, not b's (b is blocked by a)"
    );
}

#[test]
fn due_rows_on_different_threads_are_claimed_in_id_order() {
    let mut store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    let a = enqueue(&mut store, Action::Star { on: true }, vec![t1], "a");
    let b = enqueue(&mut store, Action::Star { on: true }, vec![t2], "b");
    assert_eq!(claim(&mut store, NOW + 10).unwrap().id, a);
    assert_eq!(claim(&mut store, NOW + 10).unwrap().id, b);
}

#[test]
fn a_replayed_client_token_returns_the_original_row() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let a = enqueue(&mut store, Action::Star { on: true }, vec![t1], "tok");
    let again = enqueue(&mut store, Action::Star { on: true }, vec![t1], "tok");
    assert_eq!(a, again);
    let rows: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn transient_failures_back_off_and_give_up_after_the_cap() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let id = enqueue(&mut store, Action::Archive {}, vec![t1], "a");
    let mut at = NOW + 10;
    for attempt in 1..MAX_ATTEMPTS {
        claim(&mut store, at).unwrap();
        let fail = OutboxWrite::Fail {
            id,
            failure: Failure::Transient {
                error: "Gmail is busy".into(),
                retry_at: at + 60,
            },
        };
        let outcome = store.outbox_write(&fail, at).unwrap();
        assert!(
            matches!(outcome, OutboxOutcome::Failed { gave_up: false, .. }),
            "{attempt}"
        );
        assert!(
            claim(&mut store, at + 59).is_none(),
            "not before the backoff"
        );
        at += 60;
    }
    claim(&mut store, at).unwrap();
    let last = OutboxWrite::Fail {
        id,
        failure: Failure::Transient {
            error: "Gmail is busy".into(),
            retry_at: at + 60,
        },
    };
    let OutboxOutcome::Failed {
        gave_up: true,
        row: Some(row),
    } = store.outbox_write(&last, at).unwrap()
    else {
        panic!("the 20th failure gives up");
    };
    assert_eq!(row.state, OutboxState::Failed);
    assert_eq!(row.attempts, MAX_ATTEMPTS);
    assert!(
        row.last_error
            .unwrap()
            .starts_with("gave up after 20 attempts")
    );
    assert_eq!(store.outbox_depth(1).unwrap(), (0, 1));
}

#[test]
fn undo_refuses_claimed_rows_and_recover_requeues_them() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let a = enqueue(&mut store, Action::Archive {}, vec![t1], "a");
    claim(&mut store, NOW + 10).unwrap();
    let outcome = store
        .outbox_write(&OutboxWrite::Undo { ids: vec![a, 999] }, NOW + 10)
        .unwrap();
    assert_eq!(
        outcome,
        OutboxOutcome::Undone {
            undone: vec![],
            too_late: vec![a, 999]
        }
    );
    let recovered = store
        .outbox_write(&OutboxWrite::Recover { account_id: 1 }, NOW + 11)
        .unwrap();
    assert_eq!(recovered, OutboxOutcome::Recovered(1));
    let row = claim(&mut store, NOW + 11).unwrap();
    assert_eq!(
        (row.id, row.attempts),
        (a, 0),
        "the same row, no attempt counted"
    );
}

#[test]
fn depth_due_and_reads_follow_the_rows() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    assert_eq!(store.outbox_next_due(1).unwrap(), None);
    enqueue(&mut store, Action::Archive {}, vec![t1], "a");
    assert_eq!(store.outbox_depth(1).unwrap(), (1, 0));
    assert_eq!(store.outbox_next_due(1).unwrap(), Some(NOW + 10));
    assert_eq!(store.pending_for(&[t1]).unwrap().len(), 1);
    assert!(store.failed_rows(1).unwrap().is_empty());
    assert_eq!(store.thread_remotes(1, &[t1, 999]).unwrap(), ["t1"]);
    let label = super::apply_local_tests::folder(&store, "Label_1");
    assert_eq!(
        store.folder_remote(1, label).unwrap().as_deref(),
        Some("Label_1")
    );
    assert_eq!(
        store.folder_remote(2, label).unwrap(),
        None,
        "another account's folder"
    );
}

/// Account 2 with one thread `o1` (one unread message in its inbox); its local thread id.
fn other_account(store: &mut super::Store) -> i64 {
    store
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('o', 'gmail', 'o@example.com')",
            [],
        )
        .unwrap();
    let batch = crate::sync::batch::Batch {
        upserts: vec![crate::provider::mock::envelope("o-m1", "o1", 4_000)],
        ..crate::sync::batch::Batch::new(2)
    };
    store.apply(&batch, NOW).unwrap();
    thread(store, "o1")
}

fn starred(store: &super::Store, thread: i64) -> bool {
    store
        .conn()
        .query_row(
            "SELECT max(is_starred) FROM messages WHERE thread_id = ?1",
            [thread],
            |r| r.get(0),
        )
        .unwrap()
}

fn enqueue_raw(store: &mut super::Store, threads: Vec<i64>, token: &str) -> OutboxOutcome {
    let write = OutboxWrite::Enqueue(super::outbox::NewAction {
        account_id: 1,
        action: Action::Star { on: true },
        threads,
        not_before: NOW + 10,
        client_token: token.into(),
    });
    store.outbox_write(&write, NOW).unwrap()
}

/// A thread a sync deleted after the operation checked it, and another account's thread
/// (as a reused rowid would be), leave the row and are never changed.
#[test]
fn enqueue_keeps_only_the_threads_the_account_still_holds() {
    let mut store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    let foreign = other_account(&mut store);
    let gone = crate::sync::batch::Batch {
        deletes: vec!["m3".into()],
        ..crate::sync::batch::Batch::new(1)
    };
    store.apply(&gone, NOW).unwrap();
    let outcome = enqueue_raw(&mut store, vec![t1, t2, foreign], "k");
    let OutboxOutcome::Enqueued { rows, applied } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!((rows.len(), applied), (1, 1));
    let row = store.pending_for(&[t1]).unwrap().remove(0);
    assert_eq!(row.thread_ids, vec![t1], "only the held thread is kept");
    assert!(starred(&store, t1));
    assert!(
        !starred(&store, foreign),
        "another account's thread is never applied"
    );
}

#[test]
fn enqueue_of_threads_no_longer_held_writes_nothing() {
    let mut store = seeded();
    let foreign = other_account(&mut store);
    let outcome = enqueue_raw(&mut store, vec![foreign, 9_999], "k");
    assert_eq!(
        outcome,
        OutboxOutcome::Enqueued {
            rows: Vec::new(),
            applied: 0
        }
    );
    assert_eq!(store.outbox_depth(1).unwrap(), (0, 0));
    assert_eq!(store.outbox_depth(2).unwrap(), (0, 0));
    assert!(!starred(&store, foreign));
}

/// The local change itself is account-scoped: a row naming another account's thread id (a
/// reused rowid) changes nothing there.
#[test]
fn the_local_change_touches_only_the_accounts_threads() {
    let mut store = seeded();
    let foreign = other_account(&mut store);
    let tx = store.conn.transaction().unwrap();
    let priors = super::apply_local::apply(&tx, 1, &Action::Star { on: true }, &[foreign]).unwrap();
    tx.commit().unwrap();
    assert!(priors.is_empty());
    assert!(!starred(&store, foreign));
}

fn fail_for_good(store: &mut super::Store, id: i64) {
    claim(store, NOW + 10).unwrap();
    let fail = OutboxWrite::Fail {
        id,
        failure: Failure::Permanent("Gmail says not found".into()),
    };
    store.outbox_write(&fail, NOW + 10).unwrap();
}

/// Spec §3: a failed row stays until the server's state of one of its threads arrives; a
/// body fetched for the reader is not that, and other threads' rows stay.
#[test]
fn a_sync_of_the_thread_clears_its_failed_row() {
    use crate::sync::batch::Batch;
    let mut store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    let a = enqueue(&mut store, Action::Archive {}, vec![t1], "a");
    fail_for_good(&mut store, a);
    let b = enqueue(&mut store, Action::Archive {}, vec![t2], "b");
    fail_for_good(&mut store, b);
    assert_eq!(store.outbox_depth(1).unwrap(), (0, 2));
    let body = Batch {
        bodies: vec![("m1".into(), crate::provider::Body::default())],
        ..Batch::new(1)
    };
    assert_eq!(store.apply(&body, NOW).unwrap().failed_cleared, 0);
    assert_eq!(
        store.outbox_depth(1).unwrap(),
        (0, 2),
        "a body is not a sync"
    );
    let synced = Batch {
        upserts: vec![crate::provider::mock::envelope("m1", "t1", 1_000)],
        ..Batch::new(1)
    };
    assert_eq!(store.apply(&synced, NOW).unwrap().failed_cleared, 1);
    assert_eq!(store.outbox_depth(1).unwrap(), (0, 1));
    let left = store.failed_rows(1).unwrap();
    assert_eq!(left.iter().map(|r| r.id).collect::<Vec<_>>(), vec![b]);
}

#[test]
fn failed_rows_are_bounded() {
    use super::outbox::MAX_FAILED_ROWS;
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    for i in 0..=MAX_FAILED_ROWS {
        enqueue(
            &mut store,
            Action::Star { on: true },
            vec![t1],
            &format!("k{i}"),
        );
    }
    store
        .conn()
        .execute("UPDATE outbox SET state = 'failed'", [])
        .unwrap();
    assert_eq!(store.failed_rows(1).unwrap().len(), MAX_FAILED_ROWS);
}
