//! From a draft row to an `Outgoing`: the addresses and headers, the references split, files
//! and parts kept in the draft's order, a part whose message is gone.

use super::compose::{FileRef, PartRef, attach, outgoing_for};
use crate::provider::mock::MockProvider;
use crate::provider::{Mailbox, ProviderError};
use crate::store::Store;
use crate::store::drafts::{Attachment, Draft, DraftKind, DraftWrite, Recipient};
use crate::test_support::{seed_two_messages, store_with_account};

fn draft(store: &mut Store, account: i64, attachments: Vec<Attachment>) -> Draft {
    let id = store
        .draft_upsert(
            &DraftWrite {
                id: None,
                account_id: account,
                kind: DraftKind::Reply,
                thread_id: None,
                remote_thread_id: Some("t1".into()),
                in_reply_to: Some("<m1@example.com>".into()),
                references: Some("<m0@example.com>  <m1@example.com>".into()),
                to: vec![Recipient {
                    name: Some("Sam".into()),
                    address: "sam@example.com".into(),
                }],
                cc: vec![Recipient {
                    name: None,
                    address: "cc@example.com".into(),
                }],
                bcc: vec![],
                subject: "Re: hi".into(),
                body: "text\n".into(),
                attachments,
                message_id: "<d1@example.com>".into(),
                source_remote_id: None,
            },
            1,
        )
        .unwrap()
        .unwrap();
    store.draft(id).unwrap().unwrap()
}

#[test]
fn the_outgoing_carries_the_drafts_headers_and_the_senders_name() {
    let (mut store, account) = store_with_account();
    let d = draft(&mut store, account, vec![]);
    let assembly = outgoing_for(&store, &d, "me@example.com", Some("Dana")).unwrap();
    let out = assembly.mail.outgoing;
    assert_eq!(
        (out.from.as_str(), out.from_name.as_deref()),
        ("me@example.com", Some("Dana"))
    );
    assert_eq!(
        out.to,
        [Mailbox {
            name: Some("Sam".into()),
            address: "sam@example.com".into()
        }]
    );
    assert_eq!(out.cc.len(), 1);
    assert!(out.bcc.is_empty());
    assert_eq!(out.message_id, "<d1@example.com>");
    assert_eq!(out.in_reply_to.as_deref(), Some("<m1@example.com>"));
    assert_eq!(out.references, ["<m0@example.com>", "<m1@example.com>"]);
    assert_eq!(
        (out.subject.as_str(), out.body.as_str()),
        ("Re: hi", "text\n")
    );
    assert!(out.attachments.is_empty());
    assert!(assembly.files.is_empty() && assembly.parts.is_empty());
    assert!(!assembly.part_gone);
}

#[test]
fn files_and_parts_are_listed_with_the_parts_message_at_gmail() {
    let (mut store, account) = store_with_account();
    let (m1, _) = seed_two_messages(&mut store, account);
    let d = draft(
        &mut store,
        account,
        vec![
            Attachment::File {
                path: "/tmp/a.txt".into(),
                name: "a.txt".into(),
                mime: "text/plain".into(),
                size: 1,
                listed: None,
            },
            Attachment::Part {
                message: m1,
                part: "p1".into(),
                name: "a.pdf".into(),
                mime: "application/pdf".into(),
                size: 10,
            },
        ],
    );
    let assembly = outgoing_for(&store, &d, "me@example.com", None).unwrap();
    assert_eq!(
        assembly.files,
        [FileRef {
            path: "/tmp/a.txt".into(),
            name: "a.txt".into(),
            mime: "text/plain".into(),
            listed: None,
        }]
    );
    assert_eq!(
        assembly.parts,
        [PartRef {
            message_remote: "m1".into(),
            part: "p1".into(),
            name: "a.pdf".into(),
            mime: "application/pdf".into(),
            size: 10
        }]
    );
    assert_eq!(assembly.mail.outgoing.from_name, None);
}

#[test]
fn a_part_whose_message_left_the_store_is_flagged() {
    let (mut store, account) = store_with_account();
    let d = draft(
        &mut store,
        account,
        vec![Attachment::Part {
            message: 4_242,
            part: "p1".into(),
            name: "old.pdf".into(),
            mime: "application/pdf".into(),
            size: 10,
        }],
    );
    let assembly = outgoing_for(&store, &d, "me@example.com", None).unwrap();
    assert!(assembly.parts.is_empty());
    assert!(assembly.part_gone);
}

#[test]
fn no_references_is_an_empty_list() {
    let (mut store, account) = store_with_account();
    let mut d = draft(&mut store, account, vec![]);
    d.references = None;
    let assembly = outgoing_for(&store, &d, "me@example.com", None).unwrap();
    assert!(assembly.mail.outgoing.references.is_empty());
}

/// An outside name that would split at `": "` and carries a bidi override.
const AWKWARD_NAME: &str = "a: b\u{202e}.txt";

fn message_of(error: &ProviderError) -> (&'static str, &str) {
    match error {
        ProviderError::Permanent { message, .. } => ("permanent", message),
        ProviderError::Transient { message, .. } => ("transient", message),
        other => panic!("unexpected error {other:?}"),
    }
}

#[tokio::test]
async fn an_unreadable_file_is_a_permanent_error_without_its_name() {
    let (mut store, account) = store_with_account();
    let d = draft(
        &mut store,
        account,
        vec![Attachment::File {
            path: "/nonexistent/rail-test/file".into(),
            name: AWKWARD_NAME.into(),
            mime: "text/plain".into(),
            size: 1,
            listed: None,
        }],
    );
    let assembly = outgoing_for(&store, &d, "me@example.com", None).unwrap();
    let error = attach(assembly, &MockProvider::new(), "Gmail")
        .await
        .unwrap_err();
    let (class, message) = message_of(&error);
    assert_eq!(class, "permanent");
    assert_eq!(message, "could not read an attachment of this message");
    assert!(!format!("{error:?}").contains("a: b"));
    assert!(!error.detail().unwrap_or_default().contains("a: b"));
}

#[tokio::test]
async fn a_forwarded_part_gone_from_gmail_is_permanent_without_its_name() {
    let (mut store, account) = store_with_account();
    let (message, _) = seed_two_messages(&mut store, account);
    let d = draft(
        &mut store,
        account,
        vec![Attachment::Part {
            message,
            part: "p1".into(),
            name: AWKWARD_NAME.into(),
            mime: "text/plain".into(),
            size: 1,
        }],
    );
    let assembly = outgoing_for(&store, &d, "me@example.com", None).unwrap();
    let error = attach(assembly, &MockProvider::new(), "Gmail")
        .await
        .unwrap_err();
    let (class, message) = message_of(&error);
    assert_eq!(class, "permanent");
    assert_eq!(
        message,
        "an attachment of this message is no longer in Gmail"
    );
}

#[tokio::test]
async fn a_forwarded_part_whose_message_left_the_store_is_permanent_without_its_name() {
    let (mut store, account) = store_with_account();
    let d = draft(
        &mut store,
        account,
        vec![Attachment::Part {
            message: 4_242,
            part: "p1".into(),
            name: AWKWARD_NAME.into(),
            mime: "text/plain".into(),
            size: 1,
        }],
    );
    let assembly = outgoing_for(&store, &d, "me@example.com", None).unwrap();
    let error = attach(assembly, &MockProvider::new(), "Gmail")
        .await
        .unwrap_err();
    let (class, message) = message_of(&error);
    assert_eq!(class, "permanent");
    assert_eq!(
        message,
        "the message an attachment was forwarded from is no longer in the store"
    );
}

#[test]
fn the_mail_carries_the_drafts_kind_source_copy_and_thread() {
    let (mut store, account) = store_with_account();
    let mut d = draft(&mut store, account, vec![]);
    d.source_remote_id = Some("m1".into());
    d.remote_draft_id = Some("g1".into());
    d.remote_message_id = Some("g1".into());
    let mail = outgoing_for(&store, &d, "me@example.com", None)
        .unwrap()
        .mail;
    assert_eq!(mail.kind, DraftKind::Reply);
    assert_eq!(mail.source.as_deref(), Some("m1"));
    assert_eq!(
        mail.remote,
        Some(crate::provider::DraftIds {
            draft_id: "g1".into(),
            message_id: "g1".into()
        })
    );
    assert_eq!(mail.thread.as_deref(), Some("t1"));
    d.remote_draft_id = None;
    let mail = outgoing_for(&store, &d, "me@example.com", None)
        .unwrap()
        .mail;
    assert_eq!(
        mail.remote, None,
        "not at the provider without its draft id"
    );
}

#[tokio::test]
async fn a_graph_forwards_parts_are_carried_without_being_fetched() {
    let (mut store, account) = store_with_account();
    let (message, _) = seed_two_messages(&mut store, account);
    let mut d = draft(
        &mut store,
        account,
        vec![Attachment::Part {
            message,
            part: "p1".into(),
            name: "a.pdf".into(),
            mime: "application/pdf".into(),
            size: 10,
        }],
    );
    d.kind = DraftKind::Forward;
    let mock = MockProvider::new();
    mock.with(|m| m.graph_model = true);
    let assembly = outgoing_for(&store, &d, "me@example.com", None).unwrap();
    let mail = attach(assembly, &mock, "Microsoft 365").await.unwrap();
    let attachment = &mail.outgoing.attachments[0];
    assert!(attachment.bytes.is_empty());
    assert_eq!(
        attachment.carried,
        Some(crate::provider::Carried {
            message: "m1".into(),
            part: "p1".into(),
            size: 10
        })
    );
    assert!(mock.calls().is_empty(), "nothing fetched");
}

#[tokio::test]
async fn a_forwarded_part_gone_names_the_accounts_provider() {
    let (mut store, account) = store_with_account();
    let (message, _) = seed_two_messages(&mut store, account);
    let d = draft(
        &mut store,
        account,
        vec![Attachment::Part {
            message,
            part: "p1".into(),
            name: AWKWARD_NAME.into(),
            mime: "text/plain".into(),
            size: 1,
        }],
    );
    let assembly = outgoing_for(&store, &d, "me@example.com", None).unwrap();
    let error = attach(assembly, &MockProvider::new(), "Microsoft 365")
        .await
        .unwrap_err();
    assert_eq!(
        message_of(&error),
        (
            "permanent",
            "an attachment of this message is no longer in Microsoft 365"
        )
    );
}
