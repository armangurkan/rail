//! What a bulk select reads: the threads a list query matches as keys and flags only — no
//! subject, no participant — the cap, the scope and the filter it honours, and the sender of a
//! thread's newest message.

use rusqlite::params;

use super::Store;
use super::filter::parse;
use super::view::{AccountFilter, ListQuery, PageAt, Scope, ThreadRow, where_clause};
use super::view_targets::targets_statement;
use crate::provider::mock::envelope;
use crate::provider::{Envelope, FolderKind, Mailbox, RemoteFolder};
use crate::sync::batch::Batch;

const MINUTE_MS: i64 = 60_000;
const BASE_MS: i64 = 1_760_000_000_000;
const NOW: i64 = 1_760_100_000;

fn folders() -> Vec<RemoteFolder> {
    vec![RemoteFolder {
        parent: None,
        remote_id: "INBOX".into(),
        name: "Inbox".into(),
        kind: FolderKind::Inbox,
    }]
}

/// Accounts 1 (`work`) and 2 (`home`), each with an inbox, and no mail.
fn empty() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .conn()
        .execute_batch(
            "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', 'w@example.com');
             INSERT INTO accounts (name, kind, address) VALUES ('home', 'gmail', 'h@example.com');",
        )
        .unwrap();
    for account in [1, 2] {
        let batch = Batch {
            folders: Some(folders()),
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

/// `count` one-message threads `{prefix}t{n}` in the inbox, a minute apart, newest last.
fn threads(prefix: &str, count: i64) -> Vec<Envelope> {
    (0..count)
        .map(|n| {
            envelope(
                &format!("{prefix}m{n}"),
                &format!("{prefix}t{n}"),
                BASE_MS + n * MINUTE_MS,
            )
        })
        .collect()
}

fn query(scope: Scope) -> ListQuery {
    ListQuery {
        scope,
        filter: parse(""),
        saved: None,
        account: AccountFilter::Any,
        utc_offset: 0,
    }
}

fn unified() -> ListQuery {
    query(Scope::Unified {
        accounts: vec![1, 2],
    })
}

/// Every thread the query matches, as the list draws them.
fn drawn(store: &Store, query: &ListQuery) -> Vec<ThreadRow> {
    store.thread_page(query, PageAt::First).unwrap().rows
}

/// The thread ids `matching_targets` answers, in its own order.
fn target_ids(store: &Store, query: &ListQuery, limit: usize) -> Vec<i64> {
    store
        .matching_targets(query, limit)
        .unwrap()
        .iter()
        .map(|(key, ..)| key.thread_id)
        .collect()
}

#[test]
fn matching_targets_returns_keys_and_flags_only() {
    let mut store = empty();
    // Flags that differ per thread, so a wrong projection order cannot pass by accident.
    let mut mail = threads("w", 5);
    mail[0].is_starred = true;
    mail[3].is_starred = true;
    mail[4].is_read = true;
    put(&mut store, 1, mail);

    let query = query(Scope::Inbox { account_id: 1 });
    let targets = store.matching_targets(&query, 10).unwrap();
    let rows = drawn(&store, &query);
    assert_eq!(targets.len(), 5, "every matching thread");
    assert!(
        rows.iter().any(|r| r.starred) && rows.iter().any(|r| r.unread_count == 0),
        "the fixture varies both flags: {rows:?}"
    );
    let wanted: Vec<(i64, i64, i64, bool, bool)> = rows
        .iter()
        .map(|r| {
            (
                r.key.last_date,
                r.key.thread_id,
                r.account_id,
                r.starred,
                r.unread_count > 0,
            )
        })
        .collect();
    let got: Vec<(i64, i64, i64, bool, bool)> = targets
        .iter()
        .map(|(key, account, starred, unread)| {
            (key.last_date, key.thread_id, *account, *starred, *unread)
        })
        .collect();
    assert_eq!(got, wanted, "newest first, with the list's own flags");

    // The privacy assertion is on the projection: a `subject:` term legitimately puts
    // `m.subject` in the WHERE, so only the columns read are pinned.
    let (base, values) = where_clause(&query);
    let (sql, _) = targets_statement(&base, &values, 10);
    let projection = sql
        .split("FROM threads t")
        .next()
        .unwrap()
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ");
    assert_eq!(
        projection,
        "SELECT t.last_date, t.id, t.account_id, t.is_starred, t.unread_count > 0"
    );
}

#[test]
fn matching_targets_honours_the_cap_the_scope_and_the_filter() {
    let mut store = empty();
    // Only the newest thread of each account stays unread.
    let mut work = threads("w", 7);
    for mail in &mut work[..6] {
        mail.is_read = true;
    }
    let mut home = threads("h", 2);
    home[0].is_read = true;
    put(&mut store, 1, work);
    put(&mut store, 2, home);

    let inbox = query(Scope::Inbox { account_id: 1 });
    let newest: Vec<i64> = drawn(&store, &inbox)
        .iter()
        .take(3)
        .map(|r| r.key.thread_id)
        .collect();
    assert_eq!(target_ids(&store, &inbox, 3), newest, "the three newest");
    assert_eq!(
        target_ids(&store, &inbox, 100).len(),
        7,
        "the scope leaves home's threads out"
    );

    let nobody = ListQuery {
        account: AccountFilter::Nobody,
        ..unified()
    };
    assert!(
        store.matching_targets(&nobody, 100).unwrap().is_empty(),
        "`account:` named no configured account"
    );

    let unread = ListQuery {
        filter: parse("is:unread"),
        ..unified()
    };
    let shown: Vec<i64> = drawn(&store, &unread)
        .iter()
        .map(|r| r.key.thread_id)
        .collect();
    assert_eq!(shown.len(), 2, "both accounts' unread threads: {shown:?}");
    assert_eq!(
        target_ids(&store, &unread, 100),
        shown,
        "the same threads the filtered list draws"
    );
}

#[test]
fn newest_sender_is_the_last_messages_from() {
    let mut store = empty();
    let from = |id: &str, ms: i64, address: &str| Envelope {
        from: vec![Mailbox {
            name: None,
            address: address.to_string(),
        }],
        ..envelope(id, "t1", ms)
    };
    put(
        &mut store,
        1,
        vec![
            from("m0", BASE_MS, "a@x"),
            from("m1", BASE_MS + MINUTE_MS, "b@x"),
            from("m2", BASE_MS + 2 * MINUTE_MS, "a@x"),
        ],
    );
    let thread: i64 = store
        .conn()
        .query_row("SELECT id FROM threads WHERE remote_id = 't1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        store.newest_sender(thread).unwrap().map(|m| m.address),
        Some("a@x".to_string()),
        "the newest message's From, not the thread's participants"
    );
    assert_eq!(
        store.newest_sender(thread + 999).unwrap(),
        None,
        "no such thread"
    );

    // Both providers drop an address-less `From`, and a hand-edited store can hold a
    // `from_json` that is not JSON at all: `rollup.rs`'s `unwrap_or_default` path.
    for bad in ["[]", "not json"] {
        store
            .conn()
            .execute("UPDATE messages SET from_json = ?1", params![bad])
            .unwrap();
        assert_eq!(store.newest_sender(thread).unwrap(), None, "{bad}");
    }

    // A `"` in the address comes back as stored; FR-5's round-trip check is what refuses it.
    store
        .conn()
        .execute(
            r#"UPDATE messages SET from_json = '[{"address":"\"dana reyes\"@x"}]'"#,
            params![],
        )
        .unwrap();
    assert_eq!(
        store.newest_sender(thread).unwrap().map(|m| m.address),
        Some("\"dana reyes\"@x".to_string())
    );
}

#[test]
fn newest_sender_skips_the_accounts_own_messages() {
    let mut store = empty();
    let batch = Batch {
        folders: Some(vec![
            folders().remove(0),
            RemoteFolder {
                parent: None,
                remote_id: "SENT".into(),
                name: "Sent".into(),
                kind: FolderKind::Sent,
            },
        ]),
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    let from = |id: &str, thread: &str, ms: i64, address: &str, folder: &str| Envelope {
        from: vec![Mailbox {
            name: None,
            address: address.to_string(),
        }],
        folders: vec![folder.into()],
        ..envelope(id, thread, ms)
    };
    put(
        &mut store,
        1,
        vec![
            from("m0", "t1", BASE_MS, "a@x", "INBOX"),
            // Sent from an alias the store does not know as the account's address.
            from("m1", "t1", BASE_MS + MINUTE_MS, "alias@x", "SENT"),
            // The account's own address, in another case, outside Sent (a copy to self).
            from(
                "m2",
                "t1",
                BASE_MS + 2 * MINUTE_MS,
                "W@Example.COM",
                "INBOX",
            ),
            from("n0", "t2", BASE_MS, "w@example.com", "SENT"),
            from("n1", "t2", BASE_MS + MINUTE_MS, "w@example.com", "INBOX"),
        ],
    );
    let thread = |remote: &str| -> i64 {
        store
            .conn()
            .query_row(
                "SELECT id FROM threads WHERE remote_id = ?1",
                [remote],
                |r| r.get(0),
            )
            .unwrap()
    };
    let (t1, t2) = (thread("t1"), thread("t2"));
    assert_eq!(
        store.newest_sender(t1).unwrap().map(|m| m.address),
        Some("a@x".to_string()),
        "the newest message someone else sent"
    );
    assert_eq!(
        store.newest_sender(t2).unwrap(),
        None,
        "only the user's own messages: no sender"
    );
}
