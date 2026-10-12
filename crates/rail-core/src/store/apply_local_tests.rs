//! The optimistic local change: every action and its exact reverse, overlapping actions,
//! undo and failure of the first of two, the conflict rule after a sync batch, and id reuse.

use rusqlite::params;

use super::Store;
use super::outbox::{Failure, NewAction, OutboxOutcome, OutboxWrite};
use crate::ops::{Action, RowToken};
use crate::provider::mock::envelope;
use crate::provider::{FolderKind, RemoteFolder};
use crate::sync::batch::Batch;

const NOW: i64 = 1_760_000_000;

type MakeAction = Box<dyn Fn(&Store) -> Action>;

pub(super) fn folders() -> Vec<RemoteFolder> {
    let folder = |id: &str, name: &str, kind| RemoteFolder {
        remote_id: id.into(),
        name: name.into(),
        kind,
    };
    vec![
        folder("INBOX", "Inbox", FolderKind::Inbox),
        folder("TRASH", "Trash", FolderKind::Trash),
        folder("Label_1", "Projects", FolderKind::User),
        folder("Label_2", "Receipts", FolderKind::User),
    ]
}

/// Account 1 with threads t1 (two unread messages) and t2 (one), all in the inbox.
pub(super) fn seeded() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('w', 'gmail', 'w@example.com')",
            [],
        )
        .unwrap();
    let batch = Batch {
        folders: Some(folders()),
        upserts: vec![
            envelope("m1", "t1", 1_000),
            envelope("m2", "t1", 2_000),
            envelope("m3", "t2", 3_000),
        ],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    store
}

pub(super) fn thread(store: &Store, remote: &str) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT id FROM threads WHERE remote_id = ?1",
            [remote],
            |r| r.get(0),
        )
        .unwrap()
}

pub(super) fn folder(store: &Store, remote: &str) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT id FROM folders WHERE remote_id = ?1",
            [remote],
            |r| r.get(0),
        )
        .unwrap()
}

/// Every message of the account: (remote id, is_read, is_starred, sorted folder remote ids).
pub(super) fn state(store: &Store) -> Vec<(String, bool, bool, Vec<String>)> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT m.remote_id, m.is_read, m.is_starred,
               COALESCE((SELECT group_concat(f.remote_id, ',') FROM
                 (SELECT f.remote_id FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                  WHERE mf.message_id = m.id ORDER BY f.remote_id) f), '')
             FROM messages m ORDER BY m.remote_id",
        )
        .unwrap();
    stmt.query_map([], |r| {
        let folders: String = r.get(3)?;
        Ok((
            r.get(0)?,
            r.get(1)?,
            r.get(2)?,
            folders
                .split(',')
                .filter(|f| !f.is_empty())
                .map(str::to_string)
                .collect(),
        ))
    })
    .unwrap()
    .map(Result::unwrap)
    .collect()
}

pub(super) fn enqueue(store: &mut Store, action: Action, threads: Vec<i64>, token: &str) -> i64 {
    let write = OutboxWrite::Enqueue(NewAction {
        account_id: 1,
        action,
        threads,
        draft: None,
        not_before: NOW + 10,
        client_token: token.into(),
    });
    match store.outbox_write(&write, NOW).unwrap() {
        OutboxOutcome::Enqueued { rows, .. } => rows[0],
        other => panic!("{other:?}"),
    }
}

/// The client token row `id` was queued with; empty for a row the outbox does not hold.
pub(super) fn token_of(store: &Store, id: i64) -> String {
    store
        .conn()
        .query_row("SELECT client_token FROM outbox WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap_or_default()
}

/// Undo `ids` with the tokens they were queued with.
fn undo(store: &mut Store, ids: Vec<i64>) -> OutboxOutcome {
    let rows = ids
        .into_iter()
        .map(|row| RowToken {
            row,
            client_token: token_of(store, row),
        })
        .collect();
    store
        .outbox_write(&OutboxWrite::Undo { rows }, NOW)
        .unwrap()
}

fn unread_count(store: &Store, t: i64) -> i64 {
    store
        .conn()
        .query_row("SELECT unread_count FROM threads WHERE id = ?1", [t], |r| {
            r.get(0)
        })
        .unwrap()
}

#[test]
fn every_action_applies_and_its_undo_restores_the_exact_prior_state() {
    let label = |s: &Store| folder(s, "Label_1");
    let actions: Vec<MakeAction> = vec![
        Box::new(|_| Action::Archive {}),
        Box::new(|_| Action::Trash {}),
        Box::new(|_| Action::Star { on: true }),
        Box::new(|_| Action::Read { on: true }),
        Box::new(move |s| Action::Labels {
            add: vec![label(s)],
            remove: vec![],
        }),
        Box::new(move |s| Action::Move { to: label(s) }),
    ];
    for (i, make) in actions.iter().enumerate() {
        let mut store = seeded();
        let before = state(&store);
        let t1 = thread(&store, "t1");
        let action = make(&store);
        let id = enqueue(&mut store, action.clone(), vec![t1], &format!("tok{i}"));
        let after = state(&store);
        assert_ne!(after, before, "{action:?} changed something");
        assert_eq!(after[2], before[2], "{action:?} left t2 alone");
        assert!(
            matches!(undo(&mut store, vec![id]), OutboxOutcome::Undone { ref undone, .. } if undone == &[id])
        );
        assert_eq!(state(&store), before, "{action:?} undone exactly");
        assert_eq!(unread_count(&store, t1), 2, "rollups follow the revert");
    }
}

#[test]
fn the_actions_change_what_the_spec_says() {
    let mut store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    enqueue(&mut store, Action::Trash {}, vec![t1], "a");
    enqueue(&mut store, Action::Read { on: true }, vec![t2], "b");
    let s = state(&store);
    assert_eq!(s[0].3, ["TRASH"], "trash adds Trash and removes the inbox");
    assert!(s[2].1, "read on");
    assert_eq!(unread_count(&store, t2), 0);
    let p = folder(&store, "Label_1");
    enqueue(&mut store, Action::Move { to: p }, vec![t2], "c");
    assert_eq!(
        state(&store)[2].3,
        ["Label_1"],
        "move adds the label and removes the inbox"
    );
}

#[test]
fn a_failed_apply_leaves_no_row() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    enqueue(&mut store, Action::Archive {}, vec![t1], "same");
    let before = state(&store);
    // The local change runs, then the insert is refused and the whole transaction rolls back.
    store
        .conn()
        .execute_batch(
            "CREATE TEMP TRIGGER refuse BEFORE INSERT ON outbox BEGIN SELECT RAISE(ABORT, 'test'); END;",
        )
        .unwrap();
    let write = OutboxWrite::Enqueue(NewAction {
        account_id: 1,
        action: Action::Star { on: true },
        threads: vec![t1],
        draft: None,
        not_before: 0,
        client_token: "other".into(),
    });
    assert!(store.outbox_write(&write, NOW).is_err());
    assert_eq!(state(&store), before);
    let rows: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn undo_of_an_archive_does_not_bring_back_the_inbox_a_later_move_removed() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let p = folder(&store, "Label_1");
    let archive = enqueue(&mut store, Action::Archive {}, vec![t1], "a");
    enqueue(&mut store, Action::Move { to: p }, vec![t1], "b");
    undo(&mut store, vec![archive]);
    let s = state(&store);
    assert_eq!(s[0].3, ["Label_1"], "the move's label stays, no inbox");
    assert_eq!(s[1].3, ["Label_1"]);
    assert_eq!(s[2].3, ["INBOX"], "t2 untouched");
}

#[test]
fn a_permanent_failure_of_star_on_keeps_the_later_star_off() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let on = enqueue(&mut store, Action::Star { on: true }, vec![t1], "a");
    let off = enqueue(&mut store, Action::Star { on: false }, vec![t1], "b");
    store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + 20)
        .unwrap();
    let fail = OutboxWrite::Fail {
        id: on,
        failure: Failure::Permanent("Gmail refused the request (HTTP 400)".into()),
    };
    assert!(matches!(
        store.outbox_write(&fail, NOW + 20).unwrap(),
        OutboxOutcome::Failed { gave_up: true, .. }
    ));
    let s = state(&store);
    assert!(!s[0].2 && !s[1].2, "unstarred, as the second row set it");
    let pending = store.pending_for(&[t1]).unwrap();
    assert_eq!(pending.iter().map(|r| r.id).collect::<Vec<_>>(), [off]);
    // Undoing the second row now must leave the message as it was seeded (unstarred), not
    // restore the star the failed row once set.
    undo(&mut store, vec![off]);
    assert!(!state(&store)[0].2, "undo of off: unstarred as seeded");
    // Retry brings the star back.
    let retried = store
        .outbox_write(
            &OutboxWrite::Retry {
                id: on,
                client_token: token_of(&store, on),
            },
            NOW + 30,
        )
        .unwrap();
    assert_eq!(retried, OutboxOutcome::Retried(true));
    assert!(state(&store)[0].2 && state(&store)[1].2);
}

#[test]
fn pending_rows_are_reapplied_after_a_sync_batch_and_undo_still_restores() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let id = enqueue(&mut store, Action::Archive {}, vec![t1], "a");
    // Gmail has not seen the archive yet: the next pass restates m1 in the inbox, and a new
    // message arrives in t1.
    let batch = Batch {
        upserts: vec![envelope("m1", "t1", 1_000), envelope("m9", "t1", 9_000)],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW + 1).unwrap();
    let s = state(&store);
    assert!(
        s.iter().filter(|m| m.0 != "m3").all(|m| m.3.is_empty()),
        "still archived: {s:?}"
    );
    undo(&mut store, vec![id]);
    let s = state(&store);
    assert!(
        s.iter().all(|m| m.3 == ["INBOX"]),
        "the new message is restored too: {s:?}"
    );
}

#[test]
fn a_failed_row_is_not_reapplied() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let id = enqueue(&mut store, Action::Archive {}, vec![t1], "a");
    store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + 20)
        .unwrap();
    let fail = OutboxWrite::Fail {
        id,
        failure: Failure::Permanent("no".into()),
    };
    store.outbox_write(&fail, NOW + 20).unwrap();
    let batch = Batch {
        upserts: vec![envelope("m1", "t1", 1_000)],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW + 21).unwrap();
    assert_eq!(state(&store)[0].3, ["INBOX"]);
}

#[test]
fn a_thread_the_delta_deletes_leaves_the_outbox_and_its_id_is_not_reused() {
    let mut store = seeded();
    let t2 = thread(&store, "t2");
    let id = enqueue(&mut store, Action::Star { on: true }, vec![t2], "a");
    let gone = Batch {
        deletes: vec!["m3".into()],
        ..Batch::new(1)
    };
    store.apply(&gone, NOW + 1).unwrap();
    let rows: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM outbox WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(rows, 0, "a row left with no thread is deleted");
    // The next batch may reuse t2's rowid for a new thread: the row must not touch it.
    let fresh = Batch {
        upserts: vec![envelope("m4", "t4", 4_000)],
        ..Batch::new(1)
    };
    store.apply(&fresh, NOW + 2).unwrap();
    assert!(state(&store).iter().all(|m| !m.2), "nothing was starred");
    assert!(matches!(
        undo(&mut store, vec![id]),
        OutboxOutcome::Undone { .. }
    ));
    assert!(state(&store).iter().all(|m| !m.2));
}

#[test]
fn a_revert_skips_a_message_whose_id_now_belongs_to_another_thread() {
    let mut store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    let id = enqueue(&mut store, Action::Read { on: true }, vec![t2], "a");
    // Hand-move m3 to t1, as a reused rowid would look: the revert must leave it alone.
    store
        .conn()
        .execute(
            "UPDATE messages SET thread_id = ?1 WHERE remote_id = 'm3'",
            params![t1],
        )
        .unwrap();
    undo(&mut store, vec![id]);
    assert!(state(&store)[2].1, "m3 kept is_read: it is no longer t2's");
}

#[test]
fn a_row_left_with_no_thread_is_deleted_and_a_two_thread_row_keeps_the_other() {
    let mut store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    let pending = enqueue(&mut store, Action::Star { on: true }, vec![t2], "a");
    let failed = enqueue(&mut store, Action::Read { on: true }, vec![t2], "b");
    let both = enqueue(&mut store, Action::Archive {}, vec![t1, t2], "c");
    // Row `failed` is claimed after `pending` completes its turn: fail it for good.
    store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + 20)
        .unwrap();
    store
        .outbox_write(&OutboxWrite::Complete { id: pending }, NOW + 20)
        .unwrap();
    let enqueued_pending = enqueue(&mut store, Action::Star { on: false }, vec![t2], "d");
    store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + 20)
        .unwrap();
    let fail = OutboxWrite::Fail {
        id: failed,
        failure: Failure::Permanent("no".into()),
    };
    store.outbox_write(&fail, NOW + 20).unwrap();
    let gone = Batch {
        deletes: vec!["m3".into()],
        ..Batch::new(1)
    };
    store.apply(&gone, NOW + 21).unwrap();
    let ids: Vec<i64> = store
        .conn()
        .prepare("SELECT id FROM outbox ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(ids, [both], "{failed} {enqueued_pending} are gone");
    let threads: String = store
        .conn()
        .query_row("SELECT thread_ids FROM outbox WHERE id = ?1", [both], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(threads, format!("[{t1}]"));
    assert!(store.failed_rows(1).unwrap().is_empty());
}
