//! The optimistic local change: every action and its exact reverse, overlapping actions,
//! undo and failure of the first of two, the conflict rule after a sync batch, and id reuse.

use rusqlite::params;

use super::Store;
use super::outbox::{Failure, NewAction, OutboxOutcome, OutboxWrite};
use crate::ops::Action;
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
        not_before: NOW + 10,
        client_token: token.into(),
    });
    match store.outbox_write(&write, NOW).unwrap() {
        OutboxOutcome::Enqueued { rows, .. } => rows[0],
        other => panic!("{other:?}"),
    }
}

fn undo(store: &mut Store, ids: Vec<i64>) -> OutboxOutcome {
    store.outbox_write(&OutboxWrite::Undo { ids }, NOW).unwrap()
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
    // Account 99 does not exist: the local change runs, then the row's foreign key refuses the
    // insert and the whole transaction rolls back.
    let write = OutboxWrite::Enqueue(NewAction {
        account_id: 99,
        action: Action::Star { on: true },
        threads: vec![t1],
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
fn undo_of_the_first_of_two_overlapping_actions_keeps_the_second() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let p = folder(&store, "Label_1");
    let label = enqueue(
        &mut store,
        Action::Labels {
            add: vec![p],
            remove: vec![],
        },
        vec![t1],
        "a",
    );
    let archive = enqueue(&mut store, Action::Archive {}, vec![t1], "b");
    undo(&mut store, vec![label]);
    let s = state(&store);
    assert_eq!(s[0].3, Vec::<String>::new(), "archived, without the label");
    assert_eq!(s[1].3, Vec::<String>::new());
    undo(&mut store, vec![archive]);
    assert_eq!(state(&store), state(&seeded()), "both undone: as seeded");
}

#[test]
fn a_permanent_failure_of_the_first_row_keeps_the_second() {
    let mut store = seeded();
    let t1 = thread(&store, "t1");
    let star = enqueue(&mut store, Action::Star { on: true }, vec![t1], "a");
    enqueue(&mut store, Action::Read { on: true }, vec![t1], "b");
    let claim = OutboxWrite::Claim { account_id: 1 };
    store.outbox_write(&claim, NOW + 20).unwrap();
    let fail = OutboxWrite::Fail {
        id: star,
        failure: Failure::Permanent("Gmail refused the request (HTTP 400)".into()),
    };
    assert!(matches!(
        store.outbox_write(&fail, NOW + 20).unwrap(),
        OutboxOutcome::Failed { gave_up: true, .. }
    ));
    let s = state(&store);
    assert!(!s[0].2 && !s[1].2, "the star is reverted");
    assert!(s[0].1 && s[1].1, "the read stays");
    // Retry brings the star back on top.
    let retried = store
        .outbox_write(&OutboxWrite::Retry { id: star }, NOW + 30)
        .unwrap();
    assert_eq!(retried, OutboxOutcome::Retried(true));
    let s = state(&store);
    assert!(s[0].2 && s[0].1);
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
    let ids: String = store
        .conn()
        .query_row("SELECT thread_ids FROM outbox WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(ids, "[]", "the deleted thread left the row");
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
