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
    let id = store.draft_upsert(&write(account_id), 1_000)?;
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
    assert_eq!(store.draft_upsert(&again, 2_000)?, id, "same row");
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
    let id = store.draft_upsert(&write(account_id), 1).unwrap();
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
fn draft_state_and_remote_ids_are_set_in_place() -> Result<(), StoreError> {
    let (mut store, account_id) = store_with_account();
    let id = store.draft_upsert(&write(account_id), 1)?;
    store.draft_set_state(id, DraftState::Queued)?;
    assert_eq!(store.draft(id)?.unwrap().state, DraftState::Queued);
    store.draft_set_remote(id, "d9", "m9")?;
    let draft = store.draft(id)?.unwrap();
    assert_eq!(draft.remote_draft_id.as_deref(), Some("d9"));
    assert_eq!(draft.remote_message_id.as_deref(), Some("m9"));
    Ok(())
}
