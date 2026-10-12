//! The Drafts folder: local drafts and Gmail's drafts merged newest first, a pushed draft
//! listed once, the account filter and the cap; a draft found by its Gmail message; and the
//! import source a Gmail draft is opened from.

use super::Store;
use super::drafts::{DraftKind, DraftWrite, Recipient};
use super::drafts_folder::{DraftsRowKind, MAX_DRAFTS_ROWS};
use crate::ops::DraftId;
use crate::provider::mock::envelope;
use crate::provider::{AttachmentMeta, Body, Envelope, Mailbox};
use crate::sync::batch::Batch;
use crate::test_support::store_with_account;

fn mailbox(address: &str) -> Mailbox {
    Mailbox {
        name: None,
        address: address.into(),
    }
}

/// A local draft of `account_id` saved at `at` (Unix seconds, as the writer's clock).
fn local(store: &mut Store, account_id: i64, n: i64, at: i64) -> DraftId {
    let write = DraftWrite {
        id: None,
        account_id,
        kind: DraftKind::New,
        thread_id: None,
        remote_thread_id: None,
        in_reply_to: None,
        references: None,
        to: vec![Recipient {
            name: None,
            address: format!("to{n}@example.com"),
        }],
        cc: vec![],
        bcc: vec![],
        subject: format!("local {n}"),
        body: String::new(),
        attachments: vec![],
        message_id: format!("<a{account_id}-d{n}@example.com>"),
    };
    store.draft_upsert(&write, at).unwrap().unwrap()
}

/// A synced Gmail draft `remote` in its own thread, its `Date:` at `secs`.
fn gmail_draft(remote: &str, secs: i64) -> Envelope {
    Envelope {
        is_draft: true,
        to: vec![mailbox("gmail@example.com")],
        ..envelope(remote, &format!("t-{remote}"), secs * 1000)
    }
}

fn sync(store: &mut Store, account_id: i64, upserts: Vec<Envelope>, bodies: Vec<(String, Body)>) {
    let batch = Batch {
        upserts,
        bodies,
        ..Batch::new(account_id)
    };
    store.apply(&batch, 1_760_000_000).unwrap();
}

fn message(store: &Store, account_id: i64, remote: &str) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT id FROM messages WHERE account_id = ?1 AND remote_id = ?2",
            rusqlite::params![account_id, remote],
            |r| r.get(0),
        )
        .unwrap()
}

fn second_account(store: &Store) -> i64 {
    store
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('home', 'gmail', 'h@example.com')",
            [],
        )
        .unwrap();
    store.conn().last_insert_rowid()
}

#[test]
fn the_folder_lists_local_and_gmail_drafts_and_a_pushed_one_once() {
    let (mut store, account) = store_with_account();
    let never_pushed = local(&mut store, account, 1, 1_000);
    let pushed = local(&mut store, account, 2, 3_000);
    store.draft_set_remote(pushed, "r1", "dm1").unwrap();
    sync(
        &mut store,
        account,
        vec![gmail_draft("dm1", 3_000), gmail_draft("dm2", 2_000)],
        vec![],
    );
    let dm2 = message(&store, account, "dm2");
    let rows = store.drafts_folder(&[account]).unwrap();
    let kinds: Vec<DraftsRowKind> = rows.iter().map(|r| r.kind).collect();
    assert_eq!(
        kinds,
        vec![
            DraftsRowKind::Local {
                draft: pushed,
                on_gmail: true
            },
            DraftsRowKind::Gmail { message: dm2 },
            DraftsRowKind::Local {
                draft: never_pushed,
                on_gmail: false
            },
        ],
        "newest first; dm1 is the pushed draft's copy, not a row of its own"
    );
    assert_eq!(
        rows.iter().map(|r| r.date).collect::<Vec<_>>(),
        vec![3_000, 2_000, 1_000]
    );
    assert_eq!(rows[1].subject, "subject dm2");
    assert_eq!(rows[1].to[0].address, "gmail@example.com");
    assert_eq!(rows[2].subject, "local 1");
    assert_eq!(rows[2].to[0].address, "to1@example.com");
    assert!(rows.iter().all(|r| r.account_id == account));
}

/// `messages.date` (the `Date:` header) and `drafts.updated_at` (the writer's clock) are both
/// Unix seconds: they compare as they are, no conversion.
#[test]
fn a_gmail_draft_sorts_between_local_drafts_by_seconds() {
    let (mut store, account) = store_with_account();
    let older = local(&mut store, account, 1, 1_760_000_000 - 1);
    let newer = local(&mut store, account, 2, 1_760_000_000 + 1);
    sync(
        &mut store,
        account,
        vec![gmail_draft("dm1", 1_760_000_000)],
        vec![],
    );
    let rows = store.drafts_folder(&[account]).unwrap();
    let dm1 = message(&store, account, "dm1");
    assert_eq!(
        rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        vec![
            DraftsRowKind::Local {
                draft: newer,
                on_gmail: false
            },
            DraftsRowKind::Gmail { message: dm1 },
            DraftsRowKind::Local {
                draft: older,
                on_gmail: false
            },
        ]
    );
    assert_eq!(rows[1].date, 1_760_000_000, "seconds, as stored");
}

#[test]
fn the_folder_is_filtered_by_account_and_capped() {
    let (mut store, first) = store_with_account();
    let second = second_account(&store);
    let mine = local(&mut store, first, 1, 10);
    local(&mut store, second, 1, 20);
    sync(&mut store, second, vec![gmail_draft("dx", 30)], vec![]);
    let rows = store.drafts_folder(&[first]).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        vec![DraftsRowKind::Local {
            draft: mine,
            on_gmail: false
        }]
    );
    assert_eq!(store.drafts_folder(&[first, second]).unwrap().len(), 3);
    assert!(store.drafts_folder(&[]).unwrap().is_empty());
    for n in 2..=501 {
        local(&mut store, first, n, 100 + n);
    }
    let rows = store.drafts_folder(&[first]).unwrap();
    assert_eq!(rows.len(), MAX_DRAFTS_ROWS);
    assert_eq!(rows[0].date, 601, "the newest are kept");
    // The merge is capped too: a Gmail draft newer than every local one pushes the oldest out.
    sync(&mut store, first, vec![gmail_draft("dy", 700)], vec![]);
    let dy = message(&store, first, "dy");
    let rows = store.drafts_folder(&[first]).unwrap();
    assert_eq!(rows.len(), MAX_DRAFTS_ROWS);
    assert_eq!(rows[0].kind, DraftsRowKind::Gmail { message: dy });
    assert_eq!(rows[MAX_DRAFTS_ROWS - 1].date, 103, "the two oldest went");
}

#[test]
fn a_draft_is_found_by_its_gmail_message_in_its_account_only() {
    let (mut store, first) = store_with_account();
    let second = second_account(&store);
    let draft = local(&mut store, first, 1, 10);
    store.draft_set_remote(draft, "r1", "dm1").unwrap();
    assert_eq!(
        store.draft_by_remote_message(first, "dm1").unwrap(),
        Some(draft)
    );
    assert_eq!(store.draft_by_remote_message(second, "dm1").unwrap(), None);
    assert_eq!(store.draft_by_remote_message(first, "dm2").unwrap(), None);
}

#[test]
fn the_import_source_carries_threading_bcc_and_the_draft_flag() {
    let (mut store, account) = store_with_account();
    let draft = Envelope {
        message_id: Some("<dm1@example.com>".into()),
        in_reply_to: Some("<parent@example.com>".into()),
        references: vec!["<root@example.com>".into(), "<parent@example.com>".into()],
        cc: vec![mailbox("cc@example.com")],
        bcc: vec![mailbox("bcc@example.com")],
        ..gmail_draft("dm1", 5_000)
    };
    let body = Body {
        text: "the draft text".into(),
        attachments: vec![AttachmentMeta {
            part_id: "p1".into(),
            filename: "a.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 10,
        }],
        ..Body::default()
    };
    sync(&mut store, account, vec![draft], vec![("dm1".into(), body)]);
    let id = message(&store, account, "dm1");
    let source = store.import_source(id).unwrap().expect("stored");
    assert_eq!(
        (source.message, source.account_id, source.is_draft),
        (id, account, true)
    );
    assert_eq!(
        (source.remote_thread_id.as_str(), source.remote_id.as_str()),
        ("t-dm1", "dm1")
    );
    let thread: i64 = store
        .conn()
        .query_row("SELECT thread_id FROM messages WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(source.thread_id, thread);
    assert_eq!(source.message_id.as_deref(), Some("<dm1@example.com>"));
    assert_eq!(source.in_reply_to.as_deref(), Some("<parent@example.com>"));
    assert_eq!(
        source.references,
        vec!["<root@example.com>", "<parent@example.com>"]
    );
    assert_eq!(source.subject, "subject dm1");
    assert_eq!(source.to, vec![mailbox("gmail@example.com")]);
    assert_eq!(source.cc, vec![mailbox("cc@example.com")]);
    assert_eq!(source.bcc, vec![mailbox("bcc@example.com")]);
    assert_eq!(source.body_text.as_deref(), Some("the draft text"));
    assert_eq!(source.attachments.len(), 1);
    assert_eq!(source.attachments[0].part_id, "p1");
    assert_eq!(source.attachments[0].filename, "a.pdf");
    let shown = format!("{source:?}");
    for secret in ["dm1@", "bcc@", "draft text", "a.pdf", "subject"] {
        assert!(!shown.contains(secret), "{shown}");
    }
    assert!(store.import_source(id + 100).unwrap().is_none());
    store
        .conn()
        .execute_batch("PRAGMA foreign_keys = OFF; DELETE FROM threads; PRAGMA foreign_keys = ON;")
        .unwrap();
    assert!(
        store.import_source(id).unwrap().is_none(),
        "a message of no thread"
    );
}

#[test]
fn a_drafts_row_debug_shows_no_content() {
    let (mut store, account) = store_with_account();
    local(&mut store, account, 1, 10);
    let shown = format!("{:?}", store.drafts_folder(&[account]).unwrap());
    for secret in ["to1@", "local 1"] {
        assert!(!shown.contains(secret), "{shown}");
    }
    assert!(shown.contains("account_id"), "{shown}");
}
