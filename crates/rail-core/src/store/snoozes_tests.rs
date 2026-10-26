//! Snoozes in the store: the migration; a snooze and its archive; the archived messages recorded
//! when the archive is sent; undo, refusal, retry and a settling sync through the archive row;
//! snoozing again; the wake, its rows and the woken thread's date; what pruning keeps; an
//! incoming message waking a snooze early; and the clear.

use super::Store;
use super::apply_local_tests::{folders, seeded, state, thread};
use super::outbox::{Failure, NewAction, OutboxOutcome, OutboxWrite};
use super::snoozes::{SnoozeNew, SnoozeState};
use crate::ops::{Action, RowToken};
use crate::provider::mock::envelope;
use crate::provider::{Envelope, FolderKind, RemoteFolder};
use crate::sync::batch::{Batch, Prune, Sweep};

pub(super) const NOW: i64 = 1_760_000_000;
pub(super) const HOUR: i64 = 3_600;

/// Snooze `threads` of account 1 until `until` at `NOW`, the archive due at `NOW + 10`.
pub(super) fn snooze(
    store: &mut Store,
    threads: Vec<i64>,
    until: i64,
    token: &str,
) -> (Vec<i64>, usize) {
    let write = OutboxWrite::Snooze(SnoozeNew {
        account_id: 1,
        threads,
        until,
        not_before: NOW + 10,
        client_token: token.into(),
    });
    match store.outbox_write(&write, NOW).unwrap() {
        OutboxOutcome::Snoozed { rows, snoozed } => (rows, snoozed),
        other => panic!("{other:?}"),
    }
}

/// Claim account 1's next row at `at` and record it sent; its id.
pub(super) fn send_next(store: &mut Store, at: i64) -> i64 {
    let row = match store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, at)
        .unwrap()
    {
        OutboxOutcome::Claimed(Some(row)) => row,
        other => panic!("{other:?}"),
    };
    let done = OutboxWrite::Complete {
        id: row.id,
        draft_consumed: false,
    };
    store.outbox_write(&done, at).unwrap();
    row.id
}

pub(super) fn wake(store: &mut Store, at: i64) -> Vec<i64> {
    match store
        .outbox_write(&OutboxWrite::Wake { account_id: 1 }, at)
        .unwrap()
    {
        OutboxOutcome::Woke { threads } => threads,
        other => panic!("{other:?}"),
    }
}

/// The live rows on `thread`: (action, threads, `not_before`).
pub(super) fn live(store: &Store, thread: i64) -> Vec<(Action, Vec<i64>, i64)> {
    store
        .pending_for(&[thread])
        .unwrap()
        .into_iter()
        .map(|r| (r.action, r.thread_ids, r.not_before))
        .collect()
}

/// Account 1's snoozes as (thread, until, state, `woken_at`, messages).
pub(super) type Seen = (String, i64, SnoozeState, Option<i64>, Vec<String>);

pub(super) fn snoozes(store: &Store) -> Vec<Seen> {
    store
        .snoozes(1)
        .unwrap()
        .into_iter()
        .map(|s| (s.thread_remote, s.until, s.state, s.woken_at, s.messages))
        .collect()
}

pub(super) fn names(ids: &[&str]) -> Vec<String> {
    ids.iter().map(|s| (*s).to_string()).collect()
}

/// A sync batch of `upserts` for account 1, written at `at`.
pub(super) fn sync(store: &mut Store, upserts: Vec<Envelope>, at: i64) {
    let batch = Batch {
        upserts,
        ..Batch::new(1)
    };
    store.apply(&batch, at).unwrap();
}

pub(super) fn in_folders(id: &str, thread: &str, ms: i64, folders: &[&str]) -> Envelope {
    Envelope {
        folders: names(folders),
        ..envelope(id, thread, ms)
    }
}

/// The messages of the account with their folders, read and starred flags.
pub(super) fn folders_of(store: &Store, id: &str) -> Vec<String> {
    state(store)
        .into_iter()
        .find(|m| m.0 == id)
        .map(|m| m.3)
        .unwrap()
}

pub(super) fn last_date(store: &Store, thread: i64) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT last_date FROM threads WHERE id = ?1",
            [thread],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn migration_ten_adds_the_snooze_tables_to_a_version_nine_store() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rail.db");
    drop(Store::open_at_version(&path, 9).unwrap());
    let store = Store::open(&path).unwrap();
    let version: i64 = store
        .conn()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 10);
    for table in ["snoozes", "snooze_messages"] {
        let found: bool = store
            .conn()
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
                [table],
                |r| r.get(0),
            )
            .unwrap();
        assert!(found, "{table}");
    }
}

#[test]
fn snoozing_an_inbox_thread_archives_it_at_once_and_leaves_a_pending_snooze() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let (rows, snoozed) = snooze(&mut store, vec![t1], NOW + HOUR, "a");
    assert_eq!((rows.len(), snoozed), (1, 1));
    assert_eq!(
        live(&store, t1),
        vec![(Action::Archive {}, vec![t1], NOW + 10)]
    );
    assert_eq!(folders_of(&store, "m1"), Vec::<String>::new());
    assert_eq!(folders_of(&store, "m2"), Vec::<String>::new());
    assert_eq!(
        snoozes(&store),
        vec![("t1".into(), NOW + HOUR, SnoozeState::Pending, None, vec![])]
    );
}

#[test]
fn the_archive_being_sent_makes_the_snooze_active_and_records_the_archived_messages() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    assert_eq!(
        snoozes(&store),
        vec![(
            "t1".into(),
            NOW + HOUR,
            SnoozeState::Active,
            None,
            names(&["m1", "m2"])
        )]
    );
}

#[test]
fn a_message_that_arrives_before_the_archive_is_sent_is_archived_and_recorded_too() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    sync(&mut store, vec![envelope("m4", "t1", 4_000)], NOW + 5);
    assert!(
        folders_of(&store, "m4").is_empty(),
        "the waiting archive applies to it"
    );
    assert_eq!(
        snoozes(&store)[0].1,
        NOW + HOUR,
        "a pending snooze is not woken"
    );
    send_next(&mut store, NOW + 10);
    assert_eq!(snoozes(&store)[0].4, names(&["m1", "m2", "m4"]));
}

#[test]
fn undoing_the_archive_removes_the_pending_snooze() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let (rows, _) = snooze(&mut store, vec![t1], NOW + HOUR, "a");
    let undo = OutboxWrite::Undo {
        rows: vec![RowToken {
            row: rows[0],
            client_token: "a".into(),
        }],
    };
    store.outbox_write(&undo, NOW + 1).unwrap();
    assert_eq!(snoozes(&store), []);
    assert_eq!(folders_of(&store, "m1"), names(&["INBOX"]));
}

#[test]
fn a_refused_archive_fails_the_snooze_and_a_retry_brings_it_back() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let (rows, _) = snooze(&mut store, vec![t1], NOW + HOUR, "a");
    store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + 10)
        .unwrap();
    let refuse = OutboxWrite::Fail {
        id: rows[0],
        failure: Failure::Permanent("Gmail refused the change".into()),
    };
    store.outbox_write(&refuse, NOW + 10).unwrap();
    assert_eq!(snoozes(&store)[0].2, SnoozeState::Failed);
    assert_eq!(folders_of(&store, "m1"), names(&["INBOX"]), "reverted");
    let retry = OutboxWrite::Retry {
        id: rows[0],
        client_token: "a".into(),
    };
    store.outbox_write(&retry, NOW + 20).unwrap();
    assert_eq!(snoozes(&store)[0].2, SnoozeState::Pending);
    send_next(&mut store, NOW + 20);
    assert_eq!(
        snoozes(&store),
        vec![(
            "t1".into(),
            NOW + HOUR,
            SnoozeState::Active,
            None,
            names(&["m1", "m2"])
        )]
    );
}

#[test]
fn a_sync_that_settles_the_failed_archive_removes_its_snooze() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let (rows, _) = snooze(&mut store, vec![t1], NOW + HOUR, "a");
    store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + 10)
        .unwrap();
    let refuse = OutboxWrite::Fail {
        id: rows[0],
        failure: Failure::Permanent("Gmail refused the change".into()),
    };
    store.outbox_write(&refuse, NOW + 10).unwrap();
    sync(&mut store, vec![envelope("m1", "t1", 1_000)], NOW + 30);
    assert_eq!(snoozes(&store), []);
}

#[test]
fn snoozing_an_active_snooze_again_moves_its_time_and_queues_no_second_archive() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    let (rows, snoozed) = snooze(&mut store, vec![t1], NOW + 2 * HOUR, "b");
    assert_eq!((rows, snoozed), (vec![], 1));
    assert_eq!(live(&store, t1), []);
    assert_eq!(
        snoozes(&store),
        vec![(
            "t1".into(),
            NOW + 2 * HOUR,
            SnoozeState::Active,
            None,
            names(&["m1", "m2"])
        )]
    );
}

#[test]
fn snoozing_a_pending_snooze_again_moves_its_time_and_keeps_the_one_archive() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    let (rows, snoozed) = snooze(&mut store, vec![t1], NOW + 2 * HOUR, "b");
    assert_eq!((rows, snoozed), (vec![], 1));
    assert_eq!(live(&store, t1).len(), 1);
    assert_eq!(snoozes(&store)[0].1, NOW + 2 * HOUR);
    assert_eq!(snoozes(&store)[0].2, SnoozeState::Pending);
}

#[test]
fn a_replayed_snooze_answers_with_its_archive_row_and_adds_nothing() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let first = snooze(&mut store, vec![t1], NOW + HOUR, "a");
    let again = snooze(&mut store, vec![t1], NOW + HOUR, "a");
    assert_eq!(again, first);
    assert_eq!(live(&store, t1).len(), 1);
    assert_eq!(snoozes(&store).len(), 1);
}

#[test]
fn the_wake_returns_the_recorded_messages_then_marks_the_thread_unread() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    assert_eq!(
        wake(&mut store, NOW + HOUR - 1),
        Vec::<i64>::new(),
        "not due yet"
    );
    assert_eq!(wake(&mut store, NOW + HOUR), vec![t1]);
    assert_eq!(
        live(&store, t1),
        vec![
            (
                Action::Unsnooze {
                    messages: names(&["m1", "m2"])
                },
                vec![t1],
                NOW + HOUR
            ),
            (Action::Read { on: false }, vec![t1], NOW + HOUR),
        ]
    );
    assert_eq!(folders_of(&store, "m1"), names(&["INBOX"]));
    assert_eq!(folders_of(&store, "m2"), names(&["INBOX"]));
    assert_eq!(
        snoozes(&store),
        vec![(
            "t1".into(),
            NOW + HOUR,
            SnoozeState::Woken,
            Some(NOW + HOUR),
            names(&["m1", "m2"])
        )]
    );
    assert_eq!(
        wake(&mut store, NOW + HOUR + 1),
        Vec::<i64>::new(),
        "woken once"
    );
}

#[test]
fn rows_queued_under_the_tokens_a_return_once_took_do_not_stop_the_return() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let t2 = thread(&store, "t2");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    let id = store.snoozes(1).unwrap()[0].id;
    // A client that predicts a return's tokens (once `unsnooze-{id}-{now}`) queues rows first.
    for prefix in ["unsnooze", "unsnooze-read"] {
        let row = NewAction {
            account_id: 1,
            action: Action::Star { on: true },
            threads: vec![t2],
            not_before: NOW + HOUR,
            client_token: format!("{prefix}-{id}-{}", NOW + HOUR),
        };
        store
            .outbox_write(&OutboxWrite::Enqueue(row), NOW + 20)
            .unwrap();
    }
    assert_eq!(wake(&mut store, NOW + HOUR), vec![t1]);
    let returned = Action::Unsnooze {
        messages: names(&["m1", "m2"]),
    };
    assert_eq!(
        live(&store, t1),
        vec![
            (returned, vec![t1], NOW + HOUR),
            (Action::Read { on: false }, vec![t1], NOW + HOUR),
        ]
    );
}

#[test]
fn the_wake_marks_a_thread_read_while_snoozed_unread() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    let read = OutboxWrite::Enqueue(NewAction {
        account_id: 1,
        action: Action::Read { on: true },
        threads: vec![t1],
        not_before: NOW + 20,
        client_token: "read".into(),
    });
    store.outbox_write(&read, NOW + 20).unwrap();
    send_next(&mut store, NOW + 20);
    wake(&mut store, NOW + HOUR);
    let unread: Vec<bool> = state(&store)
        .into_iter()
        .filter(|m| m.0 == "m1" || m.0 == "m2")
        .map(|m| !m.1)
        .collect();
    assert_eq!(unread, vec![true, true]);
}

#[test]
fn a_woken_thread_is_dated_at_its_wake_until_rail_archives_it_again() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    assert_eq!(last_date(&store, t1), 2, "m2's date");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    wake(&mut store, NOW + HOUR);
    assert_eq!(last_date(&store, t1), NOW + HOUR);
    let archive = OutboxWrite::Enqueue(NewAction {
        account_id: 1,
        action: Action::Archive {},
        threads: vec![t1],
        not_before: NOW + HOUR + 10,
        client_token: "again".into(),
    });
    store.outbox_write(&archive, NOW + HOUR + 1).unwrap();
    assert_eq!(snoozes(&store), [], "the woken row goes");
    assert_eq!(last_date(&store, t1), 2);
}

#[test]
fn a_due_snooze_with_nothing_left_archived_is_dropped_without_rows() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    let trashed = vec![
        in_folders("m1", "t1", 1_000, &["TRASH"]),
        in_folders("m2", "t1", 2_000, &["TRASH"]),
    ];
    sync(&mut store, trashed, NOW + 60);
    assert_eq!(wake(&mut store, NOW + HOUR), Vec::<i64>::new());
    assert_eq!(live(&store, t1), []);
    assert_eq!(snoozes(&store), []);
    assert_eq!(
        folders_of(&store, "m1"),
        names(&["TRASH"]),
        "never brought back"
    );
}

#[test]
fn the_wake_returns_only_the_recorded_messages_still_archived() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    sync(
        &mut store,
        vec![in_folders("m1", "t1", 1_000, &["TRASH"])],
        NOW + 60,
    );
    wake(&mut store, NOW + HOUR);
    assert_eq!(
        live(&store, t1)[0].0,
        Action::Unsnooze {
            messages: names(&["m2"])
        }
    );
    assert_eq!(folders_of(&store, "m1"), names(&["TRASH"]));
}

#[test]
fn pruning_and_sweeping_keep_a_snoozed_thread() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    let prune = Batch {
        prune: Some(Prune {
            window_start_ms: Some(10_000),
            search_before: 0,
        }),
        ..Batch::new(1)
    };
    store.apply(&prune, NOW + 20).unwrap();
    let ids: Vec<String> = state(&store).into_iter().map(|m| m.0).collect();
    assert_eq!(ids, names(&["m1", "m2"]), "m3 (t2) pruned, t1 kept");
    let sweep = Batch {
        sweep: Some(Sweep {
            started_at: NOW + 100,
            window_start_ms: None,
        }),
        ..Batch::new(1)
    };
    store.apply(&sweep, NOW + 100).unwrap();
    let ids: Vec<String> = state(&store).into_iter().map(|m| m.0).collect();
    assert_eq!(ids, names(&["m1", "m2"]), "unseen, but snoozed");
}

#[test]
fn an_incoming_message_on_an_active_snooze_brings_its_wake_forward() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    sync(&mut store, vec![envelope("m5", "t1", 5_000)], NOW + 100);
    assert_eq!(snoozes(&store)[0].1, NOW + 100);
    assert_eq!(store.snooze_next_due(1).unwrap(), Some(NOW + 100));
}

#[test]
fn the_users_own_sent_reply_does_not_wake_a_snooze() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    let mut all = folders();
    all.push(RemoteFolder {
        parent: None,
        remote_id: "SENT".into(),
        name: "Sent".into(),
        kind: FolderKind::Sent,
    });
    let reply = Batch {
        folders: Some(all),
        upserts: vec![in_folders("m6", "t1", 6_000, &["SENT"])],
        ..Batch::new(1)
    };
    store.apply(&reply, NOW + 100).unwrap();
    assert_eq!(snoozes(&store)[0].1, NOW + HOUR);
}

#[test]
fn a_recorded_message_returned_by_hand_does_not_wake_a_snooze() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    sync(&mut store, vec![envelope("m1", "t1", 1_000)], NOW + 100);
    assert_eq!(snoozes(&store)[0].1, NOW + HOUR);
}

#[test]
fn clearing_an_active_snooze_returns_the_thread_now_and_only_once() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let t2 = thread(&store, "t2");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    let clear = OutboxWrite::Unsnooze {
        account_id: 1,
        threads: vec![t1, t2],
    };
    assert_eq!(
        store.outbox_write(&clear, NOW + 60).unwrap(),
        OutboxOutcome::Unsnoozed { returned: 1 }
    );
    assert_eq!(live(&store, t1).len(), 2, "the return and the unread");
    assert_eq!(snoozes(&store)[0].2, SnoozeState::Woken);
    assert_eq!(snoozes(&store)[0].3, Some(NOW + 60));
    assert_eq!(
        store.outbox_write(&clear, NOW + 61).unwrap(),
        OutboxOutcome::Unsnoozed { returned: 0 }
    );
    assert_eq!(live(&store, t1).len(), 2);
}

#[test]
fn the_next_due_snooze_counts_active_snoozes_only() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let t2 = thread(&store, "t2");
    snooze(&mut store, vec![t1], NOW + 2 * HOUR, "a");
    send_next(&mut store, NOW + 10);
    snooze(&mut store, vec![t2], NOW + HOUR, "b");
    assert_eq!(store.snooze_next_due(1).unwrap(), Some(NOW + 2 * HOUR));
    assert_eq!(store.snooze_next_due(2).unwrap(), None);
}

#[test]
fn the_snooze_of_a_thread_is_read_by_its_local_id() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let t2 = thread(&store, "t2");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    let row = store.snooze_of_thread(t1).unwrap().unwrap();
    assert_eq!((row.thread_remote.as_str(), row.until), ("t1", NOW + HOUR));
    assert!(store.snooze_of_thread(t2).unwrap().is_none());
}

#[test]
fn a_message_trashed_before_its_return_is_sent_stays_in_the_trash() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    wake(&mut store, NOW + HOUR);
    // Trashed on another device before the return was sent: the sync applies the waiting
    // return again (the conflict rule), which must leave it in the trash.
    let trashed = vec![in_folders("m2", "t1", 2_000, &["TRASH"])];
    sync(&mut store, trashed, NOW + HOUR + 1);
    assert_eq!(folders_of(&store, "m2"), names(&["TRASH"]));
    assert_eq!(folders_of(&store, "m1"), names(&["INBOX"]));
}

#[test]
fn a_reply_the_user_sent_that_also_lands_in_the_inbox_does_not_wake_a_snooze() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    send_next(&mut store, NOW + 10);
    let mut all = folders();
    all.push(RemoteFolder {
        parent: None,
        remote_id: "SENT".into(),
        name: "Sent".into(),
        kind: FolderKind::Sent,
    });
    let to_self = Batch {
        folders: Some(all),
        upserts: vec![in_folders("m7", "t1", 7_000, &["INBOX", "SENT"])],
        ..Batch::new(1)
    };
    store.apply(&to_self, NOW + 100).unwrap();
    assert_eq!(snoozes(&store)[0].1, NOW + HOUR);
}

#[test]
fn clearing_a_pending_snooze_makes_it_due_at_once() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    snooze(&mut store, vec![t1], NOW + HOUR, "a");
    let clear = OutboxWrite::Unsnooze {
        account_id: 1,
        threads: vec![t1],
    };
    assert_eq!(
        store.outbox_write(&clear, NOW + 5).unwrap(),
        OutboxOutcome::Unsnoozed { returned: 1 }
    );
    assert_eq!(snoozes(&store)[0].1, NOW + 5);
    send_next(&mut store, NOW + 10);
    assert_eq!(wake(&mut store, NOW + 10), vec![t1], "back once archived");
}

#[test]
fn clearing_a_failed_snooze_drops_it() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let (rows, _) = snooze(&mut store, vec![t1], NOW + HOUR, "a");
    store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + 10)
        .unwrap();
    let refuse = OutboxWrite::Fail {
        id: rows[0],
        failure: Failure::Permanent("Gmail refused the change".into()),
    };
    store.outbox_write(&refuse, NOW + 10).unwrap();
    let clear = OutboxWrite::Unsnooze {
        account_id: 1,
        threads: vec![t1],
    };
    store.outbox_write(&clear, NOW + 20).unwrap();
    assert_eq!(snoozes(&store), Vec::<Seen>::new());
}
