//! From a draft row to an `Outgoing`: the addresses and headers, the references split, files
//! and parts kept in the draft's order, a part whose message is gone.

use super::compose::{FileRef, PartRef, outgoing_for};
use crate::provider::Mailbox;
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
    let out = assembly.outgoing;
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
    assert_eq!(assembly.part_gone, None);
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
            mime: "text/plain".into()
        }]
    );
    assert_eq!(
        assembly.parts,
        [PartRef {
            message_remote: "m1".into(),
            part: "p1".into(),
            name: "a.pdf".into(),
            mime: "application/pdf".into()
        }]
    );
    assert_eq!(assembly.outgoing.from_name, None);
}

#[test]
fn a_part_whose_message_left_the_store_is_named() {
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
    assert_eq!(assembly.part_gone.as_deref(), Some("old.pdf"));
}

#[test]
fn no_references_is_an_empty_list() {
    let (mut store, account) = store_with_account();
    let mut d = draft(&mut store, account, vec![]);
    d.references = None;
    let assembly = outgoing_for(&store, &d, "me@example.com", None).unwrap();
    assert!(assembly.outgoing.references.is_empty());
}
