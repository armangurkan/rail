//! The merged mailboxes across accounts: Starred from any folder, Sent and Trash by folder kind
//! whatever the provider calls them, paging, the filter, and their sidebar counts.

use std::collections::BTreeSet;

use super::Store;
use super::filter::parse;
use super::view::*;
use crate::provider::mock::envelope;
use crate::provider::{Envelope, FolderKind, RemoteFolder};
use crate::sync::batch::Batch;

const MINUTE_MS: i64 = 60_000;
const BASE_MS: i64 = 1_760_000_000_000;
const NOW: i64 = 1_760_100_000;

/// Account 1 shaped like Gmail (`Inbox`, `Sent`, `Trash`, `Drafts`, `Projects`) and account 2
/// like Microsoft 365 (`Inbox`, `Sent Items`, `Deleted Items`, `Inbox/Infrastructure`); no mail.
fn two_kinds() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .conn()
        .execute_batch(
            "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', 'w@example.com');
             INSERT INTO accounts (name, kind, address) VALUES ('axon', 'm365', 'a@example.com');",
        )
        .unwrap();
    let gmail = [
        ("INBOX", "Inbox", FolderKind::Inbox),
        ("SENT", "Sent", FolderKind::Sent),
        ("TRASH", "Trash", FolderKind::Trash),
        ("DRAFT", "Drafts", FolderKind::Drafts),
        ("Label_1", "Projects", FolderKind::User),
    ];
    let m365 = [
        ("in-id", "Inbox", FolderKind::Inbox),
        ("sent-id", "Sent Items", FolderKind::Sent),
        ("deleted-id", "Deleted Items", FolderKind::Trash),
        ("infra-id", "Inbox/Infrastructure", FolderKind::User),
    ];
    for (account, folders) in [(1, &gmail[..]), (2, &m365[..])] {
        let folders = folders
            .iter()
            .map(|(id, name, kind)| RemoteFolder {
                parent: None,
                remote_id: (*id).into(),
                name: (*name).into(),
                kind: *kind,
            })
            .collect();
        let batch = Batch {
            folders: Some(folders),
            ..Batch::new(account)
        };
        store.apply(&batch, NOW).unwrap();
    }
    store
}

fn put(store: &mut Store, account: i64, envelopes: Vec<Envelope>) {
    let batch = Batch {
        upserts: envelopes,
        ..Batch::new(account)
    };
    store.apply(&batch, NOW).unwrap();
}

/// One message `id` (its own thread) in `folder`, `minutes` after the base.
fn in_folder(id: &str, folder: &str, minutes: i64) -> Envelope {
    let mut e = envelope(id, &format!("t-{id}"), BASE_MS + minutes * MINUTE_MS);
    e.folders = vec![folder.into()];
    e
}

fn query(scope: Scope, filter: &str) -> ListQuery {
    ListQuery {
        scope,
        filter: parse(filter),
        saved: None,
        account: AccountFilter::Any,
        utc_offset: 0,
    }
}

fn subjects(store: &Store, q: &ListQuery) -> Vec<String> {
    let page = store.thread_page(q, PageAt::First).unwrap();
    page.rows.iter().map(|r| r.subject.clone()).collect()
}

fn starred(accounts: Vec<i64>) -> Scope {
    Scope::Starred { accounts }
}

fn kind(accounts: Vec<i64>, kind: FolderKind) -> Scope {
    Scope::Kind { accounts, kind }
}

#[test]
fn starred_finds_a_starred_thread_in_any_folder_of_any_account() {
    let mut store = two_kinds();
    let mut projects = in_folder("ws", "Label_1", 3);
    projects.is_starred = true;
    let plain = in_folder("wu", "INBOX", 4);
    put(&mut store, 1, vec![projects, plain]);
    let mut infra = in_folder("as", "infra-id", 2);
    infra.is_starred = true;
    infra.is_read = true;
    put(&mut store, 2, vec![infra]);
    assert_eq!(
        subjects(&store, &query(starred(vec![1, 2]), "")),
        ["subject ws", "subject as"],
        "newest first; the unstarred thread is not listed"
    );
    assert_eq!(
        subjects(&store, &query(starred(vec![2]), "")),
        ["subject as"]
    );
    assert_eq!(
        subjects(&store, &query(starred(vec![1, 2]), "is:unread")),
        ["subject ws"],
        "the filter still applies"
    );
    let only = ListQuery {
        account: AccountFilter::Only(2),
        ..query(starred(vec![1, 2]), "")
    };
    assert_eq!(subjects(&store, &only), ["subject as"]);
}

/// Review Focus: the kinds match, the names differ.
#[test]
fn sent_and_trash_match_by_kind_whatever_the_provider_calls_them() {
    let mut store = two_kinds();
    put(
        &mut store,
        1,
        vec![
            in_folder("wsent", "SENT", 1),
            in_folder("wtrash", "TRASH", 2),
        ],
    );
    put(
        &mut store,
        2,
        vec![
            in_folder("asent", "sent-id", 3),
            in_folder("atrash", "deleted-id", 4),
            in_folder("ainbox", "in-id", 5),
        ],
    );
    assert_eq!(
        subjects(&store, &query(kind(vec![1, 2], FolderKind::Sent), "")),
        ["subject asent", "subject wsent"]
    );
    assert_eq!(
        subjects(&store, &query(kind(vec![1, 2], FolderKind::Trash), "")),
        ["subject atrash", "subject wtrash"]
    );
    assert_eq!(
        subjects(&store, &query(kind(vec![1], FolderKind::Sent), "")),
        ["subject wsent"]
    );
}

/// Spec §4 (B2): a kind scope is read from the folder side through
/// `message_folders_by_folder`, not by a probe per thread over every message.
#[test]
fn a_kind_scope_reads_from_the_folder_side() {
    let store = two_kinds();
    let q = query(kind(vec![1, 2], FolderKind::Trash), "");
    let (base, params) = where_clause(&q);
    let (sql, values) = rows_statement(&base, &params, None, true);
    let mut stmt = store
        .conn()
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap();
    let plan: Vec<String> = stmt
        .query_map(rusqlite::params_from_iter(values.iter()), |r| r.get(3))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert!(
        plan.iter()
            .any(|step| step.contains("message_folders_by_folder")),
        "{plan:?}"
    );
    assert!(
        !plan.iter().any(|step| step.starts_with("SCAN mi")),
        "no walk over messages: {plan:?}"
    );
}

#[test]
fn starred_and_kind_scopes_page_both_ways_without_gaps() {
    let mut store = two_kinds();
    let many = |prefix: &str, folder: &str, n: i64, star: bool| -> Vec<Envelope> {
        (0..n)
            .map(|i| {
                let mut e = in_folder(&format!("{prefix}{i}"), folder, i);
                e.is_starred = star;
                e
            })
            .collect()
    };
    put(&mut store, 1, many("w", "Label_1", 70, true));
    put(&mut store, 2, many("a", "sent-id", 60, false));
    put(&mut store, 2, many("s", "infra-id", 50, true));
    for (scope, total) in [
        (starred(vec![1, 2]), 120),
        (kind(vec![1, 2], FolderKind::Sent), 60),
    ] {
        let q = query(scope, "");
        let first = store.thread_page(&q, PageAt::First).unwrap();
        let mut seen: BTreeSet<ThreadKey> = first.rows.iter().map(|r| r.key).collect();
        let mut page = first;
        while page.older {
            let last = page.rows.last().unwrap().key;
            page = store.thread_page(&q, PageAt::Older(last)).unwrap();
            for row in &page.rows {
                assert!(seen.insert(row.key), "a thread came twice");
            }
        }
        assert_eq!(seen.len(), total);
        let bottom = store.thread_page(&q, PageAt::Last).unwrap();
        assert!(!bottom.older);
    }
}

#[test]
fn sidebar_counts_the_starred_unread_threads_and_the_drafts() {
    let mut store = two_kinds();
    let mut unread_star = in_folder("ws", "Label_1", 1);
    unread_star.is_starred = true;
    let mut read_star = in_folder("wr", "INBOX", 2);
    read_star.is_starred = true;
    read_star.is_read = true;
    let plain = in_folder("wp", "INBOX", 3);
    let mut draft = in_folder("wd", "DRAFT", 4);
    draft.is_draft = true;
    put(&mut store, 1, vec![unread_star, read_star, plain, draft]);
    let mut other = in_folder("as", "infra-id", 5);
    other.is_starred = true;
    put(&mut store, 2, vec![other]);
    let counts = store.sidebar_counts(&[1, 2], &[], 0).unwrap();
    assert_eq!(
        counts.unified,
        UnifiedCounts {
            starred_unread: 2,
            drafts: 1,
            drafts_by_account: vec![(1, 1), (2, 0)],
            snoozed: 0,
        }
    );
    assert_eq!(
        store.sidebar_counts(&[2], &[], 0).unwrap().unified.drafts,
        0
    );
}
