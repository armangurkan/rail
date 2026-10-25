//! What a saved search reads: `Scope::All` (every folder of the accounts asked for), the saved
//! filter ANDed under the live one, the resolved `account:` and the sidebar's unread counts.
//! AC-12, AC-13, AC-25.

use super::Store;
use super::filter::{Filter, parse};
use super::view::*;
use crate::provider::mock::envelope;
use crate::provider::{Envelope, FolderKind, Mailbox, RemoteFolder};
use crate::sync::batch::Batch;

const MINUTE_MS: i64 = 60_000;
const BASE_MS: i64 = 1_760_000_000_000;
const NOW: i64 = 1_760_100_000;

/// `work` (id 1) and `home` (id 2), each with Inbox, Archive and Trash; no mail.
fn two_accounts() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .conn()
        .execute_batch(
            "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', 'w@example.com');
             INSERT INTO accounts (name, kind, address) VALUES ('home', 'gmail', 'h@example.com');",
        )
        .unwrap();
    let folders: Vec<RemoteFolder> = [
        ("INBOX", "Inbox", FolderKind::Inbox),
        ("ARCHIVE", "Archive", FolderKind::Archive),
        ("TRASH", "Trash", FolderKind::Trash),
    ]
    .iter()
    .map(|(id, name, kind)| RemoteFolder {
        parent: None,
        remote_id: (*id).into(),
        name: (*name).into(),
        kind: *kind,
    })
    .collect();
    for account in [1, 2] {
        let batch = Batch {
            folders: Some(folders.clone()),
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

/// One message in `folder`, `minutes` after the base, from Dana or not, read or not.
fn mail(id: &str, folder: &str, minutes: i64, dana: bool, read: bool) -> Envelope {
    let mut e = envelope(id, &format!("t-{id}"), BASE_MS + minutes * MINUTE_MS);
    e.folders = vec![folder.into()];
    e.is_read = read;
    if dana {
        e.from = vec![Mailbox {
            name: Some("Dana Reyes".into()),
            address: "dana@corp.example".into(),
        }];
    }
    e
}

fn query(scope: Scope, saved: Option<Filter>, filter: &str, account: AccountFilter) -> ListQuery {
    ListQuery {
        scope,
        filter: parse(filter),
        saved,
        account,
        utc_offset: 0,
    }
}

/// The subjects a query lists, newest first.
fn subjects(store: &Store, query: &ListQuery) -> Vec<String> {
    store
        .thread_page(query, PageAt::First)
        .unwrap()
        .rows
        .iter()
        .map(|t| t.subject.clone())
        .collect()
}

/// AC-12 (FR-10, FR-12): a saved search spans every folder of every account asked for, and the
/// live filter ANDs on top as its own `EXISTS` — so the rows are the intersection, not either
/// filter's own set.
#[test]
fn scope_all_spans_every_folder_and_ands_the_saved_filter() {
    let mut store = two_accounts();
    put(
        &mut store,
        1,
        vec![
            mail("wi", "INBOX", 0, true, false),
            mail("wa", "ARCHIVE", 1, true, false),
            mail("wt", "TRASH", 2, true, true),
            mail("wo", "INBOX", 3, false, false),
        ],
    );
    put(&mut store, 2, vec![mail("hi", "INBOX", 4, true, false)]);
    let all = || Scope::All {
        accounts: vec![1, 2],
    };

    // Every folder, every account: Trash and Archive are in, which `Scope::Unified` is not.
    let saved_only = query(all(), Some(parse("from:dana")), "", AccountFilter::Any);
    assert_eq!(
        subjects(&store, &saved_only),
        ["subject hi", "subject wt", "subject wa", "subject wi"],
    );

    // The live filter narrows it; the read Trash thread drops out.
    let both = query(
        all(),
        Some(parse("from:dana")),
        "is:unread",
        AccountFilter::Any,
    );
    assert_eq!(
        subjects(&store, &both),
        ["subject hi", "subject wa", "subject wi"],
    );

    // Neither filter alone gives that set: the saved one keeps `wt`, the live one keeps `wo`.
    let live_only = query(all(), None, "is:unread", AccountFilter::Any);
    assert_eq!(
        subjects(&store, &live_only),
        ["subject hi", "subject wo", "subject wa", "subject wi"],
    );

    // With no saved filter the scope is every thread of both accounts, as before.
    let bare = query(all(), None, "", AccountFilter::Any);
    assert_eq!(subjects(&store, &bare).len(), 5);
    assert_eq!(
        subjects(&store, &bare),
        subjects(
            &store,
            &query(all(), Some(Filter::default()), "", AccountFilter::Any)
        ),
        "an empty saved filter is the same as none",
    );
}

/// AC-13 (FR-13): `account:` inside a saved query narrows it, through the `AccountFilter` the
/// caller resolved — the store has no name-to-id lookup of its own and `Filter::message_sql`
/// emits no account condition, so this layer can only be handed a resolved one.
#[test]
fn account_filter_narrows_a_saved_search() {
    let mut store = two_accounts();
    put(&mut store, 1, vec![mail("wi", "INBOX", 0, true, false)]);
    put(&mut store, 2, vec![mail("hi", "INBOX", 1, true, false)]);
    let all = || Scope::All {
        accounts: vec![1, 2],
    };

    assert_eq!(
        subjects(
            &store,
            &query(all(), Some(parse("from:dana")), "", AccountFilter::Only(2))
        ),
        ["subject hi"],
    );
    assert_eq!(
        subjects(
            &store,
            &query(all(), Some(parse("from:dana")), "", AccountFilter::Only(1))
        ),
        ["subject wi"],
    );
    assert!(
        subjects(
            &store,
            &query(all(), Some(parse("from:dana")), "", AccountFilter::Nobody)
        )
        .is_empty(),
        "an `account:` naming no configured account matches nothing",
    );
}

/// AC-25 (FR-8, FR-13): one count per saved search, unread threads only, each narrowed by the
/// `AccountFilter` its own `account:` resolved to — so a saved `account:home` search's count
/// and its list agree instead of the sidebar over-counting it.
#[test]
fn saved_counts_counts_unread_threads_per_filter() {
    let mut store = two_accounts();
    // work: one read Dana thread and three unread others.
    put(
        &mut store,
        1,
        vec![
            mail("wr", "INBOX", 0, true, true),
            mail("o1", "INBOX", 1, false, false),
            mail("o2", "ARCHIVE", 2, false, false),
            mail("o3", "TRASH", 3, false, false),
        ],
    );
    // home: the one unread Dana thread.
    put(&mut store, 2, vec![mail("hd", "INBOX", 4, true, false)]);

    let ids = ids_json(&[1, 2]);
    let dana = parse("from:dana");
    let any = |filter: &Filter| vec![(filter.clone(), AccountFilter::Any)];

    assert_eq!(
        store
            .saved_counts(&ids, &[(dana.clone(), AccountFilter::Any)], 0)
            .unwrap(),
        [1],
        "one unread Dana thread; the read one does not count",
    );
    assert_eq!(
        store
            .saved_counts(&ids, &any(&Filter::default()), 0)
            .unwrap(),
        [4],
        "an empty saved filter counts every unread thread",
    );
    assert_eq!(
        store
            .saved_counts(
                &ids,
                &[
                    (dana.clone(), AccountFilter::Any),
                    (Filter::default(), AccountFilter::Any),
                ],
                0,
            )
            .unwrap(),
        [1, 4],
        "one count per filter, in order",
    );

    // The resolved `account:` narrows each count on its own: home holds the unread Dana
    // thread, work's is read.
    assert_eq!(
        store
            .saved_counts(&ids, &[(dana.clone(), AccountFilter::Only(2))], 0)
            .unwrap(),
        [1],
    );
    assert_eq!(
        store
            .saved_counts(&ids, &[(dana.clone(), AccountFilter::Only(1))], 0)
            .unwrap(),
        [0],
    );
    assert_eq!(
        store
            .saved_counts(&ids, &[(dana.clone(), AccountFilter::Nobody)], 0)
            .unwrap(),
        [0],
        "`account:` naming no configured account counts nothing, without a query",
    );

    // An account with no row in the store contributes 0 rather than failing.
    let with_ghost = ids_json(&[1, 2, 99]);
    assert_eq!(
        store
            .saved_counts(&with_ghost, &[(dana, AccountFilter::Any)], 0)
            .unwrap(),
        [1],
    );
    assert!(
        store.saved_counts(&ids, &[], 0).unwrap().is_empty(),
        "no filters, no queries",
    );

    // The sidebar read fills them positionally, and runs nothing when counts are off.
    let counts = store
        .sidebar_counts(
            &[1, 2],
            &[
                (parse("from:dana"), AccountFilter::Any),
                (Filter::default(), AccountFilter::Any),
            ],
            0,
        )
        .unwrap();
    assert_eq!(counts.saved, [1, 4]);
    assert!(
        store
            .sidebar_counts(&[1, 2], &[], 0)
            .unwrap()
            .saved
            .is_empty()
    );
}
