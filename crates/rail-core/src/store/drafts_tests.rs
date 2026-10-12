//! Drafts: a round trip and an update in place, a `Debug` that shows no content, the reply
//! source a reply is built from, and state and remote ids set in place.

use super::StoreError;
use super::drafts::{Attachment, DraftKind, DraftState, DraftWrite, Recipient};
use crate::test_support::{seed_two_messages, store_with_account};

fn write(account_id: i64) -> DraftWrite {
    DraftWrite {
        id: None,
        account_id,
        kind: DraftKind::New,
        thread_id: None,
        remote_thread_id: None,
        in_reply_to: None,
        references: None,
        to: vec![Recipient {
            name: Some("Dana".into()),
            address: "dana@example.com".into(),
        }],
        cc: vec![],
        bcc: vec![],
        subject: "Hello".into(),
        body: "Hi Dana,\n".into(),
        attachments: vec![Attachment::File {
            path: "/tmp/a.pdf".into(),
            name: "a.pdf".into(),
            mime: "application/pdf".into(),
            size: 10,
        }],
        message_id: "<11111111-2222-4333-8444-555555555555@example.com>".into(),
    }
}

#[test]
fn a_draft_round_trips_and_updates_in_place() -> Result<(), StoreError> {
    let (mut store, account_id) = store_with_account();
    let id = store
        .draft_upsert(&write(account_id), 1_000)?
        .expect("created");
    let draft = store.draft(id)?.expect("stored");
    assert_eq!(draft.kind, DraftKind::New);
    assert_eq!(draft.to[0].address, "dana@example.com");
    assert_eq!(draft.state, DraftState::Editing);
    assert_eq!(draft.updated_at, 1_000);
    assert!(matches!(draft.attachments[0], Attachment::File { .. }));
    let again = DraftWrite {
        id: Some(id),
        subject: "Hello again".into(),
        ..write(account_id)
    };
    assert_eq!(store.draft_upsert(&again, 2_000)?, Some(id), "same row");
    let draft = store.draft(id)?.expect("stored");
    assert_eq!(draft.subject, "Hello again");
    assert_eq!(draft.updated_at, 2_000);
    assert_eq!(
        draft.message_id,
        write(account_id).message_id,
        "fixed at creation"
    );
    let listed = store.drafts_for_account(account_id)?;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].subject, "Hello again");
    store.draft_delete(id)?;
    assert!(store.draft(id)?.is_none());
    Ok(())
}

#[test]
fn a_draft_debug_shows_no_content() {
    let (mut store, account_id) = store_with_account();
    let id = store.draft_upsert(&write(account_id), 1).unwrap().unwrap();
    let shown = format!("{:?}", store.draft(id).unwrap().unwrap());
    for secret in ["dana", "Hello", "Hi Dana", "a.pdf"] {
        assert!(!shown.contains(secret), "{shown}");
    }
    assert!(shown.contains("id: 1"), "{shown}");
}

#[test]
fn reply_source_carries_the_headers_the_daemon_needs() -> Result<(), StoreError> {
    let (mut store, account_id) = store_with_account();
    let (first, _second) = seed_two_messages(&mut store, account_id);
    let source = store.reply_source(first)?.expect("a message");
    assert_eq!(source.message, first);
    assert_eq!(source.account_id, account_id);
    assert_eq!(source.remote_thread_id, "t1");
    assert_eq!(source.message_id.as_deref(), Some("<m1@example.com>"));
    assert_eq!(source.subject, "subject m1");
    assert_eq!(source.from[0].address, "sender@example.com");
    assert!(source.reply_to.is_empty(), "no Reply-To stored yet");
    assert_eq!(source.attachments.len(), 1);
    assert_eq!(source.attachments[0].part_id, "p1");
    assert!(store.reply_source(999)?.is_none());
    Ok(())
}

#[test]
fn the_latest_message_of_a_thread_is_the_newest_by_date_then_id() -> Result<(), StoreError> {
    let (mut store, account_id) = store_with_account();
    let (first, second) = seed_two_messages(&mut store, account_id);
    let thread: i64 = store.conn().query_row(
        "SELECT thread_id FROM messages WHERE id = ?1",
        [first],
        |r| r.get(0),
    )?;
    assert_eq!(store.latest_message_of(thread)?, Some(second));
    store.conn().execute(
        "UPDATE messages SET date = 5 WHERE id IN (?1, ?2)",
        [first, second],
    )?;
    assert_eq!(
        store.latest_message_of(thread)?,
        Some(first.max(second)),
        "a tie goes to the higher id"
    );
    assert_eq!(store.latest_message_of(thread + 100)?, None);
    Ok(())
}

#[test]
fn draft_state_and_remote_ids_are_set_in_place() -> Result<(), StoreError> {
    let (mut store, account_id) = store_with_account();
    let id = store.draft_upsert(&write(account_id), 1)?.expect("created");
    store.draft_set_state(id, DraftState::Queued)?;
    assert_eq!(store.draft(id)?.unwrap().state, DraftState::Queued);
    store.draft_set_remote(id, "d9", "m9")?;
    let draft = store.draft(id)?.unwrap();
    assert_eq!(draft.remote_draft_id.as_deref(), Some("d9"));
    assert_eq!(draft.remote_message_id.as_deref(), Some("m9"));
    Ok(())
}

#[test]
fn an_update_of_a_draft_the_account_does_not_hold_writes_nothing() -> Result<(), StoreError> {
    let (mut store, account_id) = store_with_account();
    let id = store.draft_upsert(&write(account_id), 1)?.expect("created");
    let missing = DraftWrite {
        id: Some(id + 100),
        ..write(account_id)
    };
    assert_eq!(store.draft_upsert(&missing, 2)?, None, "no such draft");
    store.conn().execute(
        "INSERT INTO accounts (name, kind, address) VALUES ('o', 'gmail', 'o@example.com')",
        [],
    )?;
    let other = store.conn().last_insert_rowid();
    let foreign = DraftWrite {
        id: Some(id),
        subject: "taken over".into(),
        ..write(other)
    };
    assert_eq!(
        store.draft_upsert(&foreign, 3)?,
        None,
        "another account's draft"
    );
    let draft = store.draft(id)?.expect("stored");
    assert_eq!((draft.subject.as_str(), draft.updated_at), ("Hello", 1));
    Ok(())
}

/// Ids are never reused (AUTOINCREMENT), so nothing left for a deleted draft names a new one.
#[test]
fn a_deleted_drafts_id_is_never_reused() -> Result<(), StoreError> {
    let (mut store, account_id) = store_with_account();
    let first = store.draft_upsert(&write(account_id), 1)?.expect("created");
    store.draft_delete(first)?;
    let next = DraftWrite {
        message_id: "<other@example.com>".into(),
        ..write(account_id)
    };
    let second = store.draft_upsert(&next, 2)?.expect("created");
    assert!(second > first, "{second} after {first}");
    Ok(())
}

#[test]
fn a_messages_remote_id_is_read_and_an_unknown_message_is_none() -> Result<(), StoreError> {
    let (mut store, account) = store_with_account();
    let (m1, _) = seed_two_messages(&mut store, account);
    assert_eq!(store.message_remote_id(account, m1)?.as_deref(), Some("m1"));
    assert_eq!(store.message_remote_id(account, 9_999)?, None);
    assert_eq!(
        store.message_remote_id(account + 1, m1)?,
        None,
        "another account's message is not this draft's to forward"
    );
    Ok(())
}

#[test]
fn an_unparseable_attachment_list_is_reported_unreadable() -> Result<(), StoreError> {
    let (mut store, account) = store_with_account();
    let id = store.draft_upsert(&write(account), 1)?.unwrap();
    assert!(store.draft_attachments_readable(id)?);
    store.conn().execute(
        "UPDATE drafts SET attachments_json = '[{\"kind\":\"nope\"}]' WHERE id = ?1",
        [id],
    )?;
    assert!(!store.draft_attachments_readable(id)?);
    assert!(store.draft(id)?.unwrap().attachments.is_empty());
    assert!(
        store.draft_attachments_readable(9_999)?,
        "no draft, nothing to refuse"
    );
    Ok(())
}

fn save(
    store: &mut super::Store,
    write: DraftWrite,
    token: &str,
) -> Result<super::drafts::DraftSaved, StoreError> {
    use super::outbox::{OutboxOutcome, OutboxWrite};
    let save = OutboxWrite::DraftSave {
        write: Box::new(write),
        client_token: token.into(),
    };
    match store.outbox_write(&save, 2_000)? {
        OutboxOutcome::DraftSaved(saved) => Ok(saved),
        other => panic!("{other:?}"),
    }
}

/// The writer's save: a create replayed by its token answers the same draft unchanged, an
/// update of another account's draft is missing, and neither an update nor a delete touches
/// a draft being sent.
#[test]
fn the_writers_save_replays_by_token_and_leaves_a_draft_being_sent_alone() -> Result<(), StoreError>
{
    use super::drafts::DraftSaved;
    use super::outbox::{OutboxOutcome, OutboxWrite};
    let (mut store, account_id) = store_with_account();
    let DraftSaved::Saved(id) = save(&mut store, write(account_id), "t1")? else {
        panic!("created");
    };
    let replay = DraftWrite {
        subject: "Other".into(),
        message_id: "<other@example.com>".into(),
        ..write(account_id)
    };
    assert_eq!(save(&mut store, replay, "t1")?, DraftSaved::Saved(id));
    assert_eq!(store.draft(id)?.expect("kept").subject, "Hello");
    let elsewhere = DraftWrite {
        id: Some(id),
        account_id: account_id + 1,
        ..write(account_id)
    };
    assert_eq!(save(&mut store, elsewhere, "t2")?, DraftSaved::Missing);
    store.draft_set_state(id, DraftState::Queued)?;
    let update = DraftWrite {
        id: Some(id),
        ..write(account_id)
    };
    assert_eq!(save(&mut store, update, "t3")?, DraftSaved::Queued);
    let delete = OutboxWrite::DraftDelete(id);
    assert_eq!(
        store.outbox_write(&delete, 2_000)?,
        OutboxOutcome::DraftDeleted(false)
    );
    store.draft_set_state(id, DraftState::Editing)?;
    assert_eq!(
        store.outbox_write(&delete, 2_000)?,
        OutboxOutcome::DraftDeleted(true)
    );
    assert!(store.draft(id)?.is_none());
    Ok(())
}
