//! Snoozes at their edges: snoozing again after the archive failed or after mail came back;
//! clearing a failed snooze for good; a thread the server deleted; a woken thread leaving the
//! inbox through Rail or elsewhere; a replay; a thread named twice; giving up; mail to oneself;
//! and what the check every sync batch makes may cost.

use rusqlite::params;

use super::Store;
use super::apply_local_tests::{folder, folders, seeded, thread};
use super::outbox::{Failure, MAX_ATTEMPTS, NewAction, OutboxOutcome, OutboxWrite};
use super::snoozes::{INCOMING, SnoozeState};
use super::snoozes_tests::{
    HOUR, NOW, folders_of, in_folders, last_date, live, names, send_next, snooze, snoozes, sync,
    wake,
};
use crate::ops::Action;
use crate::provider::mock::envelope;
use crate::provider::{FolderKind, RemoteFolder};
use crate::sync::batch::Batch;

/// Claim account 1's next row at `at` and record `failure`; its id.
fn fail_next(store: &mut Store, at: i64, failure: Failure) -> i64 {
    let row = match store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, at)
        .unwrap()
    {
        OutboxOutcome::Claimed(Some(row)) => row.id,
        other => panic!("{other:?}"),
    };
    store
        .outbox_write(&OutboxWrite::Fail { id: row, failure }, at)
        .unwrap();
    row
}

fn refused() -> Failure {
    Failure::Permanent("Gmail refused the change".into())
}

fn failed_rows(store: &Store) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT count(*) FROM outbox WHERE state = 'failed'",
            [],
            |r| r.get(0),
        )
        .unwrap()
}

/// Rail queues `action` on `thread` (account 1).
fn act(store: &mut Store, action: Action, thread: i64, token: &str) {
    let new = NewAction {
        account_id: 1,
        action,
        threads: vec![thread],
        not_before: NOW + 2 * HOUR,
        client_token: token.into(),
    };
    store
        .outbox_write(&OutboxWrite::Enqueue(new), NOW + 2 * HOUR)
        .unwrap();
}

/// t1 snoozed until `NOW + HOUR`, its archive sent, then woken at `NOW + HOUR`.
fn woken_t1(store: &mut Store) -> i64 {
    let t1 = thread(store, "t1");
    snooze(store, vec![t1], NOW + HOUR, "a");
    send_next(store, NOW + 10);
    assert_eq!(wake(store, NOW + HOUR), [t1]);
    t1
}

#[test]
fn a_failed_snooze_snoozed_again_queues_a_new_archive() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    fail_next(&mut store, NOW + 10, refused());
    assert_eq!(folders_of(&store, "m1"), names(&["INBOX"]), "reverted");
    let (rows, snoozed) = snooze(&mut store, vec![t1], NOW + 2 * HOUR, "b");
    assert_eq!((rows.len(), snoozed), (1, 1));
    assert_eq!(failed_rows(&store), 0, "the failed archive goes");
    assert_eq!(
        live(&store, t1),
        vec![(Action::Archive {}, vec![t1], NOW + 10)]
    );
    assert_eq!(
        snoozes(&store),
        vec![(
            "t1".into(),
            NOW + 2 * HOUR,
            SnoozeState::Pending,
            None,
            vec![]
        )]
    );
    assert!(folders_of(&store, "m1").is_empty(), "archived again");
}

#[test]
fn snoozing_again_after_new_mail_puts_the_thread_back_and_snoozes_it_afresh() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    sync(&mut store, vec![envelope("m4", "t1", 4_000)], NOW + 20);
    assert_eq!(snoozes(&store)[0].1, NOW + 20, "the new mail made it due");
    let (rows, snoozed) = snooze(&mut store, vec![t1], NOW + 2 * HOUR, "b");
    assert_eq!((rows.len(), snoozed), (1, 1));
    let back = Action::Unsnooze {
        messages: names(&["m1", "m2"]),
    };
    assert_eq!(
        live(&store, t1),
        vec![
            (back, vec![t1], NOW),
            (Action::Read { on: false }, vec![t1], NOW),
            (Action::Archive {}, vec![t1], NOW + 10),
        ]
    );
    assert_eq!(
        snoozes(&store),
        vec![(
            "t1".into(),
            NOW + 2 * HOUR,
            SnoozeState::Pending,
            None,
            vec![]
        )]
    );
    for id in ["m1", "m2", "m4"] {
        assert!(folders_of(&store, id).is_empty(), "{id} is archived");
    }
    for _ in 0..3 {
        send_next(&mut store, NOW + 30);
    }
    assert_eq!(
        snoozes(&store)[0].4,
        names(&["m1", "m2", "m4"]),
        "the new archive records all of it"
    );
}

#[test]
fn snoozing_again_after_a_recorded_message_came_back_by_hand_archives_it_again() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    sync(&mut store, vec![envelope("m1", "t1", 1_000)], NOW + 20);
    assert_eq!(
        snoozes(&store)[0].1,
        NOW + HOUR,
        "recorded mail wakes nothing"
    );
    let (rows, _) = snooze(&mut store, vec![t1], NOW + 2 * HOUR, "b");
    assert_eq!(rows.len(), 1, "the message in the inbox is archived again");
    let back = Action::Unsnooze {
        messages: names(&["m2"]),
    };
    assert_eq!(live(&store, t1)[0].0, back, "only what stayed archived");
    assert_eq!(folders_of(&store, "m1"), Vec::<String>::new());
}

#[test]
fn clearing_a_failed_snooze_drops_its_archive_so_no_retry_can_send_it() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    let row = fail_next(&mut store, NOW + 10, refused());
    let clear = OutboxWrite::Unsnooze {
        account_id: 1,
        threads: vec![t1],
    };
    store.outbox_write(&clear, NOW + 20).unwrap();
    assert_eq!(snoozes(&store), []);
    assert_eq!(failed_rows(&store), 0);
    let retry = OutboxWrite::Retry {
        id: row,
        client_token: "a".into(),
    };
    let retried = store.outbox_write(&retry, NOW + 30).unwrap();
    assert!(
        matches!(retried, OutboxOutcome::Retried(false)),
        "{retried:?}"
    );
    assert_eq!(folders_of(&store, "m1"), names(&["INBOX"]));
}

#[test]
fn a_thread_the_server_deleted_takes_its_snooze_with_it() {
    let gone = Batch {
        deletes: names(&["m1", "m2"]),
        ..Batch::new(1)
    };
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    store.apply(&gone, NOW + 20).unwrap();
    assert_eq!(snoozes(&store), [], "an active snooze");
    let mut store = seeded();
    woken_t1(&mut store);
    store.apply(&gone, NOW + 2 * HOUR).unwrap();
    assert_eq!(snoozes(&store), [], "a woken one");
}

#[test]
fn trashing_moving_or_snoozing_a_woken_thread_again_drops_its_woken_row() {
    let mut store = seeded();
    let t1 = woken_t1(&mut store);
    act(&mut store, Action::Trash {}, t1, "x");
    assert_eq!(snoozes(&store), [], "trash");
    let mut store = seeded();
    let t1 = woken_t1(&mut store);
    let to = folder(&store, "Label_1");
    act(&mut store, Action::Move { to }, t1, "x");
    assert_eq!(snoozes(&store), [], "move");
    let mut store = seeded();
    let t1 = woken_t1(&mut store);
    snooze(&mut store, vec![t1], NOW + 3 * HOUR, "b");
    let states: Vec<SnoozeState> = snoozes(&store).into_iter().map(|s| s.2).collect();
    assert_eq!(states, [SnoozeState::Pending], "snoozed again");
}

#[test]
fn a_woken_thread_archived_elsewhere_is_dated_by_its_mail_again() {
    let mut store = seeded();
    let t1 = woken_t1(&mut store);
    send_next(&mut store, NOW + HOUR);
    send_next(&mut store, NOW + HOUR);
    assert_eq!(last_date(&store, t1), NOW + HOUR);
    let archived = vec![
        in_folders("m1", "t1", 1_000, &[]),
        in_folders("m2", "t1", 2_000, &[]),
    ];
    sync(&mut store, archived, NOW + 2 * HOUR);
    assert_eq!(snoozes(&store), []);
    assert_eq!(last_date(&store, t1), 2, "its newest message's");
}

#[test]
fn a_woken_thread_keeps_its_wake_date_while_its_return_still_waits() {
    let mut store = seeded();
    let t1 = woken_t1(&mut store);
    // The return is not sent yet, so the server still has the mail out of the inbox; the waiting
    // return is applied again before the woken row is judged, and the row stays.
    let archived = vec![
        in_folders("m1", "t1", 1_000, &[]),
        in_folders("m2", "t1", 2_000, &[]),
    ];
    sync(&mut store, archived, NOW + 2 * HOUR);
    let states: Vec<SnoozeState> = snoozes(&store).into_iter().map(|s| s.2).collect();
    assert_eq!(states, [SnoozeState::Woken]);
    assert_eq!(last_date(&store, t1), NOW + HOUR, "dated at its wake");
}

#[test]
fn a_replay_counts_every_thread_the_first_call_snoozed() {
    let mut store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    let first = snooze(&mut store, vec![t1, t2], NOW + 2 * HOUR, "b");
    assert_eq!(first.1, 2);
    assert_eq!(snooze(&mut store, vec![t1, t2], NOW + 2 * HOUR, "b"), first);
}

#[test]
fn a_thread_named_twice_is_snoozed_once() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let (rows, snoozed) = snooze(&mut store, vec![t1, t1], NOW + HOUR, "a");
    assert_eq!((rows.len(), snoozed), (1, 1));
    assert_eq!(snoozes(&store).len(), 1);
}

#[test]
fn an_archive_that_gives_up_fails_its_snooze() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    for attempt in 0..MAX_ATTEMPTS {
        let at = NOW + 10 + i64::from(attempt) * 100;
        let failure = Failure::Transient {
            error: "Gmail is busy".into(),
            retry_at: at + 50,
        };
        fail_next(&mut store, at, failure);
    }
    assert_eq!(snoozes(&store)[0].2, SnoozeState::Failed);
}

#[test]
fn mail_to_myself_in_the_inbox_comes_back_with_the_thread() {
    let mut store = seeded();
    let mut all = folders();
    all.push(RemoteFolder {
        parent: None,
        remote_id: "SENT".into(),
        name: "Sent".into(),
        kind: FolderKind::Sent,
    });
    let to_myself = Batch {
        folders: Some(all),
        upserts: vec![in_folders("m5", "t1", 5_000, &["INBOX", "SENT"])],
        ..Batch::new(1)
    };
    store.apply(&to_myself, NOW).unwrap();
    assert_eq!(folders_of(&store, "m5"), names(&["INBOX", "SENT"]));
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    wake(&mut store, NOW + HOUR);
    let back = Action::Unsnooze {
        messages: names(&["m1", "m2", "m5"]),
    };
    assert_eq!(live(&store, t1)[0].0, back);
}

#[test]
fn a_sync_batch_reaches_threads_and_messages_by_their_ids_not_by_the_account() {
    let store = seeded();
    let mut stmt = store
        .conn()
        .prepare(&format!("EXPLAIN QUERY PLAN {INCOMING}"))
        .unwrap();
    let plan: Vec<String> = stmt
        .query_map(params![1, "[\"m1\"]", NOW], |r| r.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    // Reading every thread or message of the account per snooze is what a batch must not pay.
    for step in &plan {
        let words: Vec<&str> = step.split_whitespace().collect();
        if let [verb, table, ..] = words.as_slice()
            && matches!(*table, "t" | "m" | "threads" | "messages")
        {
            assert!(
                *verb != "SCAN" && !step.contains("(account_id=?)"),
                "{step}\n{plan:#?}"
            );
        }
    }
}
