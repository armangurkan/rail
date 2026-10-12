//! The draft rows of the outbox: a push folded into the one still waiting, a push of a draft
//! being sent, a push's completion, discard while a push is in flight and the Gmail delete it
//! leaves, the Gmail delete a sent pushed draft leaves, an import, and the saving count.

use rusqlite::params;

use super::Store;
use super::drafts::{DraftKind, DraftSaved, DraftWrite, Recipient};
use super::outbox::{
    DraftDeleteOutcome, NewAction, OutboxCounts, OutboxOutcome, OutboxState, OutboxWrite,
};
use crate::ops::{Action, DraftId, OutboxId};
use crate::test_support::store_with_account;

fn write(account_id: i64, n: i64) -> DraftWrite {
    DraftWrite {
        id: None,
        account_id,
        kind: DraftKind::New,
        thread_id: None,
        remote_thread_id: None,
        in_reply_to: None,
        references: None,
        to: vec![Recipient {
            name: None,
            address: "to@example.com".into(),
        }],
        cc: vec![],
        bcc: vec![],
        subject: format!("draft {n}"),
        body: String::new(),
        attachments: vec![],
        message_id: format!("<od{n}@example.com>"),
    }
}

fn draft_in(store: &mut Store, account_id: i64, n: i64) -> DraftId {
    store
        .draft_upsert(&write(account_id, n), 1)
        .unwrap()
        .unwrap()
}

/// Enqueue `action` at `now` (due ten seconds later); the row ids it answers.
fn enqueue(
    store: &mut Store,
    account_id: i64,
    action: Action,
    token: &str,
    now: i64,
) -> Vec<OutboxId> {
    let new = NewAction {
        account_id,
        action,
        threads: vec![],
        not_before: now + 10,
        client_token: token.into(),
    };
    match store.outbox_write(&OutboxWrite::Enqueue(new), now).unwrap() {
        OutboxOutcome::Enqueued { rows, .. } => rows,
        other => panic!("{other:?}"),
    }
}

fn push(
    store: &mut Store,
    account_id: i64,
    draft: DraftId,
    token: &str,
    now: i64,
) -> Vec<OutboxId> {
    enqueue(store, account_id, Action::PushDraft { draft }, token, now)
}

fn claim(store: &mut Store, account_id: i64, at: i64) -> OutboxId {
    match store
        .outbox_write(&OutboxWrite::Claim { account_id }, at)
        .unwrap()
    {
        OutboxOutcome::Claimed(Some(row)) => row.id,
        other => panic!("{other:?}"),
    }
}

fn complete_push(store: &mut Store, id: OutboxId, draft: DraftId, remote: (&str, &str)) {
    let write = OutboxWrite::CompletePush {
        id,
        draft,
        remote_draft_id: remote.0.into(),
        remote_message_id: remote.1.into(),
    };
    assert_eq!(store.outbox_write(&write, 0).unwrap(), OutboxOutcome::Done);
}

fn discard(store: &mut Store, draft: DraftId, now: i64) -> DraftDeleteOutcome {
    match store
        .outbox_write(&OutboxWrite::DraftDelete(draft), now)
        .unwrap()
    {
        OutboxOutcome::DraftDeleted(outcome) => outcome,
        other => panic!("{other:?}"),
    }
}

/// Every row as `(kind of the payload, draft_id, client_token, not_before, state)`.
fn rows(store: &Store) -> Vec<(String, Option<DraftId>, String, i64, String)> {
    let mut stmt = store
        .conn()
        .prepare(
            "SELECT json_extract(payload, '$.kind'), draft_id, client_token, not_before, state
             FROM outbox ORDER BY id",
        )
        .unwrap();
    stmt.query_map([], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
    })
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

fn payload_remote(store: &Store) -> (i64, String) {
    store
        .conn()
        .query_row(
            "SELECT json_extract(payload, '$.draft'), json_extract(payload, '$.remote_draft_id')
             FROM outbox",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

#[test]
fn a_second_push_in_the_grace_answers_the_pending_row() {
    let (mut store, account) = store_with_account();
    let d = draft_in(&mut store, account, 1);
    let first = push(&mut store, account, d, "p1", 100);
    assert_eq!(first.len(), 1);
    assert_eq!(push(&mut store, account, d, "p2", 105), first, "folded");
    assert_eq!(rows(&store).len(), 1, "one push_draft row");
    let (_, _, token, not_before, _) = rows(&store)[0].clone();
    assert_eq!(
        (token.as_str(), not_before),
        ("p2", 115),
        "the newest push owns the undo and restarts the grace"
    );
    assert_eq!(
        push(&mut store, account, d, "p1", 100),
        first,
        "a replay of the first token answers the row"
    );
    assert_eq!(
        push(&mut store, account, d, "p2", 105),
        first,
        "and of the second"
    );
    assert_eq!(
        rows(&store)[0].2,
        "p2",
        "a replay does not take the undo back"
    );
    assert_eq!(rows(&store)[0].3, 115);
    let too_late = undo(&mut store, first[0], "p1");
    assert_eq!(
        too_late,
        (vec![], vec![first[0]]),
        "the first token no longer undoes it"
    );
    assert_eq!(rows(&store).len(), 1);
    assert!(
        matches!(
            store
                .outbox_write(
                    &OutboxWrite::Claim {
                        account_id: account
                    },
                    110
                )
                .unwrap(),
            OutboxOutcome::Claimed(None)
        ),
        "not due before the second push's grace ends"
    );
    assert_eq!(claim(&mut store, account, 115), first[0]);
    assert_eq!(
        push(&mut store, account, d, "p2", 105),
        first,
        "a replay of the second token while inflight is that row"
    );
    let third = push(&mut store, account, d, "p3", 116);
    assert_eq!(third.len(), 1);
    assert_ne!(third, first, "the inflight row is not folded into");
    let kinds: Vec<String> = rows(&store).into_iter().map(|r| r.0).collect();
    assert_eq!(kinds, vec!["push_draft", "push_draft"]);
}

#[test]
fn the_second_pushs_token_undoes_the_folded_row() {
    let (mut store, account) = store_with_account();
    let d = draft_in(&mut store, account, 1);
    let row = push(&mut store, account, d, "p1", 100)[0];
    push(&mut store, account, d, "p2", 105);
    assert_eq!(undo(&mut store, row, "p2"), (vec![row], vec![]));
    assert!(rows(&store).is_empty());
}

fn undo(store: &mut Store, row: OutboxId, token: &str) -> (Vec<OutboxId>, Vec<OutboxId>) {
    let write = OutboxWrite::Undo {
        rows: vec![crate::ops::RowToken {
            row,
            client_token: token.into(),
        }],
    };
    match store.outbox_write(&write, 0).unwrap() {
        OutboxOutcome::Undone { undone, too_late } => (undone, too_late),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_push_of_a_draft_being_sent_is_not_queued() {
    let (mut store, account) = store_with_account();
    let d = draft_in(&mut store, account, 1);
    assert_eq!(
        enqueue(&mut store, account, Action::Send { draft: d }, "s", 100).len(),
        1
    );
    assert!(push(&mut store, account, d, "p", 101).is_empty());
    let kinds: Vec<String> = rows(&store).into_iter().map(|r| r.0).collect();
    assert_eq!(kinds, vec!["send"]);
}

#[test]
fn complete_push_stores_the_gmail_ids_and_removes_the_row() {
    let (mut store, account) = store_with_account();
    let d = draft_in(&mut store, account, 1);
    let row = push(&mut store, account, d, "p", 100)[0];
    claim(&mut store, account, 110);
    complete_push(&mut store, row, d, ("r1", "dm1"));
    let draft = store.draft(d).unwrap().unwrap();
    assert_eq!(draft.remote_draft_id.as_deref(), Some("r1"));
    assert_eq!(draft.remote_message_id.as_deref(), Some("dm1"));
    assert!(rows(&store).is_empty());
    // A push that lands after its draft was deleted (as by a hand-edited store) only goes.
    let gone = draft_in(&mut store, account, 2);
    let row = push(&mut store, account, gone, "q", 200)[0];
    claim(&mut store, account, 210);
    store
        .conn()
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .unwrap();
    store
        .conn()
        .execute("DELETE FROM drafts WHERE id = ?1", [gone])
        .unwrap();
    store
        .conn()
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    complete_push(&mut store, row, gone, ("r2", "dm2"));
    assert!(rows(&store).is_empty());
    assert!(store.draft(gone).unwrap().is_none());
}

#[test]
fn discard_is_refused_while_a_push_is_in_flight_then_deletes_at_gmail() {
    let (mut store, account) = store_with_account();
    let d = draft_in(&mut store, account, 1);
    let first = push(&mut store, account, d, "p1", 100)[0];
    claim(&mut store, account, 110);
    complete_push(&mut store, first, d, ("r1", "dm1"));
    let second = push(&mut store, account, d, "p2", 200)[0];
    claim(&mut store, account, 210);
    assert_eq!(discard(&mut store, d, 220), DraftDeleteOutcome::BeingPushed);
    assert!(store.draft(d).unwrap().is_some(), "nothing changed");
    assert_eq!(rows(&store).len(), 1);
    assert_eq!(rows(&store)[0].4, "inflight");
    complete_push(&mut store, second, d, ("r1", "dm2"));
    assert_eq!(discard(&mut store, d, 300), DraftDeleteOutcome::Deleted);
    assert!(store.draft(d).unwrap().is_none());
    assert_eq!(
        rows(&store),
        vec![(
            "delete_draft".into(),
            None,
            format!("discard-{d}-300"),
            300,
            "pending".into()
        )]
    );
    assert_eq!(payload_remote(&store), (d, "r1".into()));
    let pending = store.outbox_row(rows_id(&store)).unwrap().unwrap();
    assert_eq!(pending.state, OutboxState::Pending);
    assert_eq!(pending.account_id, account);
    assert_eq!(
        pending.action,
        Action::DeleteDraft {
            draft: d,
            remote_draft_id: "r1".into()
        }
    );
    assert!(pending.thread_ids.is_empty());
}

fn rows_id(store: &Store) -> OutboxId {
    store
        .conn()
        .query_row("SELECT id FROM outbox", [], |r| r.get(0))
        .unwrap()
}

#[test]
fn discard_removes_pending_pushes_and_needs_no_gmail_delete_when_never_pushed() {
    let (mut store, account) = store_with_account();
    let d = draft_in(&mut store, account, 1);
    push(&mut store, account, d, "p1", 100);
    assert_eq!(discard(&mut store, d, 105), DraftDeleteOutcome::Deleted);
    assert!(store.draft(d).unwrap().is_none());
    assert!(
        rows(&store).is_empty(),
        "no Gmail delete: it never reached Gmail"
    );
    // Pushed once, a second push still pending: the push goes, the Gmail delete is queued.
    let e = draft_in(&mut store, account, 2);
    store.draft_set_remote(e, "r2", "dm2").unwrap();
    push(&mut store, account, e, "p2", 200);
    assert_eq!(discard(&mut store, e, 205), DraftDeleteOutcome::Deleted);
    let left = rows(&store);
    assert_eq!(left.len(), 1);
    assert_eq!((left[0].0.as_str(), left[0].1), ("delete_draft", None));
    assert_eq!(payload_remote(&store), (e, "r2".into()));
    assert_eq!(
        discard(&mut store, 999, 206),
        DraftDeleteOutcome::Deleted,
        "gone already"
    );
    // A draft being sent is left alone.
    let f = draft_in(&mut store, account, 3);
    enqueue(&mut store, account, Action::Send { draft: f }, "s", 300);
    assert_eq!(discard(&mut store, f, 301), DraftDeleteOutcome::BeingSent);
    assert!(store.draft(f).unwrap().is_some());
}

#[test]
fn a_completed_send_of_a_pushed_draft_queues_its_gmail_delete() {
    let (mut store, account) = store_with_account();
    let d = draft_in(&mut store, account, 1);
    store.draft_set_remote(d, "r1", "dm1").unwrap();
    let send = enqueue(&mut store, account, Action::Send { draft: d }, "s1", 100)[0];
    claim(&mut store, account, 110);
    store
        .outbox_write(&OutboxWrite::Complete { id: send }, 500)
        .unwrap();
    assert!(store.draft(d).unwrap().is_none());
    let left = rows(&store);
    assert_eq!(left.len(), 1);
    let (kind, draft_id, token, not_before, state) = left[0].clone();
    assert_eq!((kind.as_str(), draft_id), ("delete_draft", None));
    assert_eq!(token, format!("after-send-{send}"));
    assert!(not_before <= 500, "due now");
    assert_eq!(state, "pending");
    assert_eq!(payload_remote(&store), (d, "r1".into()));
    store.conn().execute("DELETE FROM outbox", []).unwrap();
    let never = draft_in(&mut store, account, 2);
    let send = enqueue(
        &mut store,
        account,
        Action::Send { draft: never },
        "s2",
        600,
    )[0];
    claim(&mut store, account, 610);
    store
        .outbox_write(&OutboxWrite::Complete { id: send }, 620)
        .unwrap();
    assert!(store.draft(never).unwrap().is_none());
    assert!(
        rows(&store).is_empty(),
        "never pushed: nothing to delete at Gmail"
    );
}

fn import(store: &mut Store, w: DraftWrite, remote: (&str, &str), token: &str) -> DraftSaved {
    let write = OutboxWrite::DraftImport {
        write: Box::new(w),
        remote_draft_id: remote.0.into(),
        remote_message_id: remote.1.into(),
        client_token: token.into(),
    };
    match store.outbox_write(&write, 1_000).unwrap() {
        OutboxOutcome::DraftSaved(saved) => saved,
        other => panic!("{other:?}"),
    }
}

#[test]
fn an_import_makes_one_draft_with_its_gmail_identity() {
    let (mut store, account) = store_with_account();
    let first = import(&mut store, write(account, 1), ("r1", "dm1"), "i1");
    let DraftSaved::Saved(id) = first else {
        panic!("{first:?}");
    };
    let again = import(&mut store, write(account, 2), ("r1", "dm1"), "i2");
    assert_eq!(again, DraftSaved::Saved(id), "the draft that import made");
    let count: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM drafts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1);
    let draft = store.draft(id).unwrap().unwrap();
    assert_eq!(draft.remote_draft_id.as_deref(), Some("r1"));
    assert_eq!(draft.remote_message_id.as_deref(), Some("dm1"));
    assert_eq!(draft.subject, "draft 1");
    assert_eq!(draft.updated_at, 1_000);
    // Another account's draft of that Gmail message is not this account's.
    store
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('home', 'gmail', 'h@example.com')",
            [],
        )
        .unwrap();
    let other = store.conn().last_insert_rowid();
    let theirs = import(&mut store, write(other, 3), ("r1", "dm1"), "i3");
    assert!(
        matches!(theirs, DraftSaved::Saved(t) if t != id),
        "{theirs:?}"
    );
}

#[test]
fn an_import_never_rewrites_a_draft_or_its_gmail_identity() {
    let (mut store, account) = store_with_account();
    // A draft saved earlier with token "t1", then pushed (its own Gmail identity).
    let saved = DraftWrite {
        message_id: "<saved@example.com>".into(),
        ..write(account, 1)
    };
    let save = OutboxWrite::DraftSave {
        write: Box::new(saved),
        client_token: "t1".into(),
    };
    let OutboxOutcome::DraftSaved(DraftSaved::Saved(mine)) = store.outbox_write(&save, 10).unwrap()
    else {
        panic!("saved");
    };
    store.draft_set_remote(mine, "r-mine", "dm-mine").unwrap();
    // An import with that token is refused: it binds nothing, and the draft keeps its ids.
    let replayed = import(&mut store, write(account, 2), ("r9", "dm9"), "t1");
    assert_eq!(replayed, DraftSaved::TokenUsed);
    // So is one in another account: tokens are unique across accounts.
    store
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('home', 'gmail', 'h@example.com')",
            [],
        )
        .unwrap();
    let other = store.conn().last_insert_rowid();
    let elsewhere = import(&mut store, write(other, 4), ("r7", "dm7"), "t1");
    assert_eq!(elsewhere, DraftSaved::TokenUsed);
    let draft = store.draft(mine).unwrap().unwrap();
    assert_eq!(draft.remote_draft_id.as_deref(), Some("r-mine"));
    assert_eq!(draft.remote_message_id.as_deref(), Some("dm-mine"));
    // An import naming a draft id is refused: it never updates an existing draft.
    let named = DraftWrite {
        id: Some(mine),
        subject: "rewritten".into(),
        ..write(account, 3)
    };
    assert_eq!(
        import(&mut store, named, ("r8", "dm8"), "t3"),
        DraftSaved::Missing
    );
    let draft = store.draft(mine).unwrap().unwrap();
    assert_eq!(draft.subject, "draft 1");
    assert_eq!(draft.remote_message_id.as_deref(), Some("dm-mine"));
}

#[test]
fn the_depth_counts_a_push_in_flight_as_saving() {
    let (mut store, account) = store_with_account();
    let d = draft_in(&mut store, account, 1);
    push(&mut store, account, d, "p", 100);
    let counts = |store: &Store| store.outbox_depth(account).unwrap();
    assert_eq!(
        counts(&store),
        OutboxCounts {
            pending: 1,
            failed: 0,
            sending: 0,
            saving: 0
        },
        "waiting, not saving yet"
    );
    claim(&mut store, account, 110);
    let e = draft_in(&mut store, account, 2);
    enqueue(&mut store, account, Action::Send { draft: e }, "s", 100);
    claim(&mut store, account, 110);
    assert_eq!(
        counts(&store),
        OutboxCounts {
            pending: 2,
            failed: 0,
            sending: 1,
            saving: 1
        }
    );
    store
        .conn()
        .execute(
            "INSERT INTO outbox (account_id, kind, payload, state, client_token, not_before,
               thread_ids, reversal, draft_id)
             VALUES (?1, 'save_draft', ?2, 'inflight', 'del', 0, '[]', '[]', NULL)",
            params![
                account,
                r#"{"kind":"delete_draft","draft":9,"remote_draft_id":"r"}"#
            ],
        )
        .unwrap();
    assert_eq!(counts(&store).saving, 1, "a delete in flight is not saving");
}

/// Gmail's draft r1 now holds dm2 (edited elsewhere) while Rail's draft of r1 still names
/// dm1: an import of dm2 answers Rail's draft ("Rail's version wins") and writes nothing.
#[test]
fn an_import_of_a_gmail_draft_another_draft_holds_answers_that_draft() {
    let (mut store, account) = store_with_account();
    let DraftSaved::Saved(mine) = import(&mut store, write(account, 1), ("r1", "dm1"), "i1") else {
        panic!("imported");
    };
    let again = import(&mut store, write(account, 2), ("r1", "dm2"), "i2");
    assert_eq!(again, DraftSaved::Saved(mine));
    let count: i64 = store
        .conn()
        .query_row("SELECT count(*) FROM drafts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 1, "no second draft of r1");
    let draft = store.draft(mine).unwrap().unwrap();
    assert_eq!(draft.subject, "draft 1", "Rail's text is kept");
    assert_eq!(draft.remote_message_id.as_deref(), Some("dm1"));
    assert_eq!(
        store.draft_client_token(mine).unwrap().as_deref(),
        Some("i1")
    );
}
