//! The Drafts folder: local drafts and Gmail's drafts merged newest first, a pushed draft
//! listed once, the account filter and the cap; a draft found by its Gmail message; and the
//! import source a Gmail draft is opened from.

use super::Store;
use super::drafts::{Attachment, DraftKind, DraftWrite, Recipient};
use super::drafts_folder::{DraftsRowKind, GMAIL, MAX_DRAFTS_ROWS};
use crate::ops::DraftId;
use crate::provider::mock::envelope;
use crate::provider::{AttachmentMeta, Body, Envelope, FolderKind, Mailbox, RemoteFolder};
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
        source_remote_id: None,
    };
    store.draft_upsert(&write, at).unwrap().unwrap()
}

/// A synced Gmail draft `remote` in its own thread and Gmail's Drafts, its `Date:` at `secs`.
fn gmail_draft(remote: &str, secs: i64) -> Envelope {
    Envelope {
        is_draft: true,
        to: vec![mailbox("gmail@example.com")],
        folders: vec!["DRAFT".into()],
        ..envelope(remote, &format!("t-{remote}"), secs * 1000)
    }
}

/// The account's folders as Gmail lists them: the inbox and the Drafts.
fn folders() -> Vec<RemoteFolder> {
    [
        ("INBOX", "Inbox", FolderKind::Inbox),
        ("DRAFT", "Drafts", FolderKind::Drafts),
    ]
    .into_iter()
    .map(|(remote_id, name, kind)| RemoteFolder {
        parent: None,
        remote_id: remote_id.into(),
        name: name.into(),
        kind,
    })
    .collect()
}

fn sync(store: &mut Store, account_id: i64, upserts: Vec<Envelope>, bodies: Vec<(String, Body)>) {
    let batch = Batch {
        folders: Some(folders()),
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
                on_remote: true
            },
            DraftsRowKind::Remote { message: dm2 },
            DraftsRowKind::Local {
                draft: never_pushed,
                on_remote: false
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
                on_remote: false
            },
            DraftsRowKind::Remote { message: dm1 },
            DraftsRowKind::Local {
                draft: older,
                on_remote: false
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
            on_remote: false
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
    assert_eq!(rows[0].kind, DraftsRowKind::Remote { message: dy });
    assert_eq!(rows[MAX_DRAFTS_ROWS - 1].date, 103, "the two oldest went");
}

/// The Gmail half reads the account's Drafts folder, not every message: a large mailbox with
/// few drafts answers only the drafts, through the folder's index.
#[test]
fn a_large_mailbox_with_few_drafts_answers_only_its_drafts_from_the_drafts_folder() {
    let (mut store, account) = store_with_account();
    let mail: Vec<Envelope> = (0..2_000)
        .map(|n| envelope(&format!("m{n}"), &format!("t{}", n % 300), 1_000 + n))
        .collect();
    let drafts = vec![gmail_draft("dm1", 10), gmail_draft("dm2", 20)];
    sync(
        &mut store,
        account,
        mail.into_iter().chain(drafts).collect(),
        vec![],
    );
    let rows = store.drafts_folder(&[account]).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
        vec![
            DraftsRowKind::Remote {
                message: message(&store, account, "dm2")
            },
            DraftsRowKind::Remote {
                message: message(&store, account, "dm1")
            },
        ]
    );
    let mut stmt = store
        .conn()
        .prepare(&format!("EXPLAIN QUERY PLAN {GMAIL}"))
        .unwrap();
    let plan: Vec<String> = stmt
        .query_map(rusqlite::params![format!("[{account}]"), 500], |r| r.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter()
            .any(|step| step.contains("message_folders_by_folder")),
        "{plan:?}"
    );
    assert!(
        !plan
            .iter()
            .any(|step| step.starts_with("SCAN m") || step.starts_with("SCAN t")),
        "no walk over messages or threads: {plan:?}"
    );
}

#[test]
fn another_accounts_draft_does_not_hide_a_gmail_draft() {
    let (mut store, first) = store_with_account();
    let second = second_account(&store);
    sync(&mut store, first, vec![gmail_draft("dm1", 50)], vec![]);
    let dm1 = message(&store, first, "dm1");
    let theirs = local(&mut store, second, 1, 10);
    store.draft_set_remote(theirs, "r1", "dm1").unwrap();
    assert_eq!(
        store
            .drafts_folder(&[first])
            .unwrap()
            .iter()
            .map(|r| r.kind)
            .collect::<Vec<_>>(),
        vec![DraftsRowKind::Remote { message: dm1 }],
        "the same Gmail id in another account is not this message"
    );
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
fn a_token_names_its_draft_and_its_gmail_message_in_any_account() {
    let (mut store, first) = store_with_account();
    let second = second_account(&store);
    let plain = local(&mut store, first, 1, 10);
    let pushed = local(&mut store, first, 2, 20);
    store.draft_set_remote(pushed, "r1", "dm1").unwrap();
    store
        .conn()
        .execute_batch(&format!(
            "UPDATE drafts SET client_token = 'a' WHERE id = {plain};
             UPDATE drafts SET client_token = 'b' WHERE id = {pushed};"
        ))
        .unwrap();
    let theirs = local(&mut store, second, 3, 30);
    store
        .conn()
        .execute(
            "UPDATE drafts SET client_token = 'z' WHERE id = ?1",
            [theirs],
        )
        .unwrap();
    assert_eq!(store.draft_token_owner("a").unwrap(), Some((plain, None)));
    assert_eq!(
        store.draft_token_owner("b").unwrap(),
        Some((pushed, Some("dm1".into())))
    );
    assert_eq!(
        store.draft_token_owner("z").unwrap(),
        Some((theirs, None)),
        "tokens are unique across accounts"
    );
    assert_eq!(store.draft_token_owner("c").unwrap(), None);
    assert_eq!(
        store.draft_client_token(plain).unwrap().as_deref(),
        Some("a")
    );
    assert_eq!(store.draft_client_token(theirs + 100).unwrap(), None);
}

#[test]
fn every_drafts_file_paths_are_listed_and_an_unreadable_list_stops_the_answer() {
    let (mut store, account) = store_with_account();
    let a = local(&mut store, account, 1, 10);
    let b = local(&mut store, account, 2, 20);
    let file = |path: &str| Attachment::File {
        path: path.into(),
        name: "n".into(),
        mime: "m".into(),
        size: 1,
        listed: None,
    };
    let part = Attachment::Part {
        message: 1,
        part: "p".into(),
        name: "n".into(),
        mime: "m".into(),
        size: 1,
    };
    for (id, list) in [
        (a, vec![file("/f/1"), part]),
        (b, vec![file("/f/2"), file("/f/3")]),
    ] {
        store
            .conn()
            .execute(
                "UPDATE drafts SET attachments_json = ?2 WHERE id = ?1",
                rusqlite::params![id, serde_json::to_string(&list).unwrap()],
            )
            .unwrap();
    }
    let mut paths = store.draft_file_paths().unwrap().unwrap();
    paths.sort();
    assert_eq!(paths, ["/f/1", "/f/2", "/f/3"]);
    store
        .conn()
        .execute(
            "UPDATE drafts SET attachments_json = 'not json' WHERE id = ?1",
            [a],
        )
        .unwrap();
    assert_eq!(
        store.draft_file_paths().unwrap(),
        None,
        "unreadable: no answer"
    );
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
            plain_file: true,
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
