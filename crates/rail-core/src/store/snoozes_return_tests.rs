//! A snoozed thread's return that fails (#106): refused or given up, it stays failed through the
//! next sync, with its woken snooze, until it is retried, the thread is back in the inbox by
//! other means, or Rail takes the thread out again; a transient failure is tried again.

use super::Store;
use super::apply_local_tests::seeded;
use super::outbox::{Failure, MAX_ATTEMPTS, OutboxOutcome, OutboxWrite};
use super::snoozes::SnoozeState;
use super::snoozes_edge_tests::{act, refused, woken_t1};
use super::snoozes_tests::{HOUR, NOW, folders_of, in_folders, last_date, names, snoozes, sync};
use crate::ops::Action;

/// Claim account 1's next row at `at`, check it is t1's return, and record `failure`.
fn fail_return(store: &mut Store, at: i64, failure: Failure) -> (i64, String) {
    let row = match store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, at)
        .unwrap()
    {
        OutboxOutcome::Claimed(Some(row)) => row,
        other => panic!("{other:?}"),
    };
    assert!(matches!(row.action, Action::Unsnooze { .. }), "{row:?}");
    store
        .outbox_write(
            &OutboxWrite::Fail {
                id: row.id,
                failure,
            },
            at,
        )
        .unwrap();
    (row.id, row.client_token)
}

/// The failed rows of account 1, as (id, action).
fn failed(store: &Store) -> Vec<(i64, Action)> {
    store
        .failed_rows(1)
        .unwrap()
        .into_iter()
        .map(|r| (r.id, r.action))
        .collect()
}

fn states(store: &Store) -> Vec<SnoozeState> {
    snoozes(store).into_iter().map(|s| s.2).collect()
}

/// A sync that finds t1's mail still archived on the server, at `at`.
fn sync_still_archived(store: &mut Store, at: i64) {
    let archived = vec![
        in_folders("m1", "t1", 1_000, &[]),
        in_folders("m2", "t1", 2_000, &[]),
    ];
    sync(store, archived, at);
}

#[test]
fn a_refused_return_survives_the_next_sync_and_a_retry_sends_it_again() {
    let mut store = seeded();
    let t1 = woken_t1(&mut store);
    let (row, token) = fail_return(&mut store, NOW + HOUR, refused());
    assert_eq!(folders_of(&store, "m1"), Vec::<String>::new(), "reverted");
    sync_still_archived(&mut store, NOW + 2 * HOUR);
    let unsnooze = Action::Unsnooze {
        messages: names(&["m1", "m2"]),
    };
    assert_eq!(failed(&store), [(row, unsnooze)], "still in :errors");
    assert_eq!(states(&store), [SnoozeState::Woken], "its snooze is kept");
    let retry = OutboxWrite::Retry {
        id: row,
        client_token: token,
    };
    let retried = store.outbox_write(&retry, NOW + 3 * HOUR).unwrap();
    assert_eq!(retried, OutboxOutcome::Retried(true));
    assert_eq!(folders_of(&store, "m1"), names(&["INBOX"]));
    assert_eq!(last_date(&store, t1), NOW + HOUR, "dated at its wake again");
}

#[test]
fn a_return_that_gives_up_stays_failed_through_the_next_sync() {
    let mut store = seeded();
    woken_t1(&mut store);
    for attempt in 0..MAX_ATTEMPTS {
        let at = NOW + HOUR + i64::from(attempt) * 100;
        let failure = Failure::Transient {
            error: "Gmail is busy".into(),
            retry_at: at + 50,
        };
        fail_return(&mut store, at, failure);
    }
    sync_still_archived(&mut store, NOW + 3 * HOUR);
    assert_eq!(failed(&store).len(), 1);
    assert_eq!(states(&store), [SnoozeState::Woken]);
}

#[test]
fn a_return_that_fails_for_a_while_is_tried_again_and_keeps_the_thread_in_the_inbox() {
    let mut store = seeded();
    let t1 = woken_t1(&mut store);
    let busy = Failure::Transient {
        error: "Gmail is busy".into(),
        retry_at: NOW + HOUR + 60,
    };
    let (row, _) = fail_return(&mut store, NOW + HOUR, busy);
    sync_still_archived(&mut store, NOW + HOUR + 30);
    assert_eq!(failed(&store), []);
    assert_eq!(states(&store), [SnoozeState::Woken]);
    assert_eq!(
        folders_of(&store, "m1"),
        names(&["INBOX"]),
        "still on its way"
    );
    let again = store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + HOUR + 60)
        .unwrap();
    let OutboxOutcome::Claimed(Some(claimed)) = again else {
        panic!("{again:?}");
    };
    assert_eq!((claimed.id, claimed.thread_ids), (row, vec![t1]));
}

#[test]
fn a_failed_return_goes_once_the_thread_is_back_in_the_inbox() {
    let mut store = seeded();
    woken_t1(&mut store);
    fail_return(&mut store, NOW + HOUR, refused());
    let back = vec![
        in_folders("m1", "t1", 1_000, &["INBOX"]),
        in_folders("m2", "t1", 2_000, &["INBOX"]),
    ];
    sync(&mut store, back, NOW + 2 * HOUR);
    assert_eq!(failed(&store), [], "the return happened elsewhere");
    assert_eq!(states(&store), [SnoozeState::Woken]);
}

#[test]
fn rail_taking_the_thread_out_again_drops_its_failed_return() {
    let mut store = seeded();
    let t1 = woken_t1(&mut store);
    fail_return(&mut store, NOW + HOUR, refused());
    act(&mut store, Action::Trash {}, t1, "trash");
    sync_still_archived(&mut store, NOW + 3 * HOUR);
    assert_eq!(failed(&store), []);
    assert_eq!(snoozes(&store), []);
}

#[test]
fn a_failed_return_goes_once_none_of_its_mail_is_still_archived() {
    let mut store = seeded();
    woken_t1(&mut store);
    fail_return(&mut store, NOW + HOUR, refused());
    let trashed = vec![
        in_folders("m1", "t1", 1_000, &["TRASH"]),
        in_folders("m2", "t1", 2_000, &["TRASH"]),
    ];
    sync(&mut store, trashed, NOW + 2 * HOUR);
    assert_eq!(failed(&store), [], "nothing left to return");
    assert_eq!(snoozes(&store), []);
}
