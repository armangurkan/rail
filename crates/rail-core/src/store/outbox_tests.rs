//! The queue: claim order and `not_before`, the client token, transient retries and the cap,
//! undo of claimed rows, recovery, depth.

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
