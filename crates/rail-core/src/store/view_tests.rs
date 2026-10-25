//! The mail screen's queries against a seeded store: every scope, both keyset edges, every
//! filter operator, dates at their boundaries, hostile strings in every column, the bounds.

use std::collections::BTreeSet;

use rusqlite::params;

use super::Store;
use super::filter::{Day, parse};
use super::view::*;
use super::view::{direction, rows_statement, where_clause};
use crate::provider::mock::envelope;
use crate::provider::{AttachmentMeta, Body, Envelope, FolderKind, Mailbox, RemoteFolder};
use crate::sync::UNREADABLE_BODY;
use crate::sync::batch::Batch;

const MINUTE_MS: i64 = 60_000;
const BASE_MS: i64 = 1_760_000_000_000;
const NOW: i64 = 1_760_100_000;

fn folders() -> Vec<RemoteFolder> {
    vec![
        RemoteFolder {
            parent: None,
            remote_id: "INBOX".into(),
            name: "Inbox".into(),
            kind: FolderKind::Inbox,
        },
        RemoteFolder {
            parent: None,
            remote_id: "Label_1".into(),
            name: "Projects".into(),
            kind: FolderKind::User,
        },
        RemoteFolder {
            parent: None,
            remote_id: "SENT".into(),
            name: "Sent".into(),
            kind: FolderKind::Sent,
        },
    ]
}

/// Accounts 1 (`work`) and 2 (`home`), each with the three folders, and no mail.
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

/// `count` one-message threads `{prefix}{n}` in the inbox, a minute apart, newest last.
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

fn filtered(text: &str) -> ListQuery {
    ListQuery {
        filter: parse(text),
        ..query(Scope::Unified {
            accounts: vec![1, 2],
        })
    }
}

fn subjects(page: &ThreadPage) -> Vec<String> {
    page.rows.iter().map(|r| r.subject.clone()).collect()
}

/// Every thread of a filtered unified inbox, by subject.
fn matching(store: &Store, text: &str) -> BTreeSet<String> {
    let page = store.thread_page(&filtered(text), PageAt::First).unwrap();
    subjects(&page).into_iter().collect()
}

#[test]
fn pages_walk_both_ways_without_gaps_or_repeats() {
    let mut store = empty();
    put(&mut store, 1, threads("w", 250));
    let q = query(Scope::Inbox { account_id: 1 });
    let first = store.thread_page(&q, PageAt::First).unwrap();
    assert_eq!(first.rows.len(), PAGE);
    assert_eq!((first.older, first.newer), (true, false));
    assert_eq!(first.rows[0].subject, "subject wm249", "newest first");
    let second = store
        .thread_page(&q, PageAt::Older(first.rows[PAGE - 1].key))
        .unwrap();
    let third = store
        .thread_page(&q, PageAt::Older(second.rows[PAGE - 1].key))
        .unwrap();
    assert_eq!(third.rows.len(), 50);
    assert_eq!((third.older, third.newer), (false, true));
    let mut seen = BTreeSet::new();
    for row in first.rows.iter().chain(&second.rows).chain(&third.rows) {
        assert!(seen.insert(row.key), "a thread came twice");
    }
    assert_eq!(seen.len(), 250);
    let last = store.thread_page(&q, PageAt::Last).unwrap();
    assert_eq!(
        last.rows.last().unwrap().subject,
        "subject wm0",
        "oldest at the bottom"
    );
    assert_eq!((last.older, last.newer), (false, true));
    let back = store
        .thread_page(&q, PageAt::Newer(last.rows[0].key))
        .unwrap();
    assert_eq!(back.rows.len(), PAGE);
    assert!(back.rows.last().unwrap().key > last.rows[0].key);
    assert_eq!((back.older, back.newer), (true, true));
    let from = store
        .thread_page(&q, PageAt::From(second.rows[0].key))
        .unwrap();
    assert_eq!(
        from.rows[0].key, second.rows[0].key,
        "From includes its anchor"
    );
}

#[test]
fn equal_dates_page_by_thread_id() {
    let mut store = empty();
    let same: Vec<Envelope> = (0..150)
        .map(|n| envelope(&format!("m{n}"), &format!("t{n}"), BASE_MS))
        .collect();
    put(&mut store, 1, same);
    let q = query(Scope::Inbox { account_id: 1 });
    let first = store.thread_page(&q, PageAt::First).unwrap();
    let rest = store
        .thread_page(&q, PageAt::Older(first.rows[PAGE - 1].key))
        .unwrap();
    let keys: BTreeSet<ThreadKey> = first.rows.iter().chain(&rest.rows).map(|r| r.key).collect();
    assert_eq!(keys.len(), 150);
    assert!(!rest.older);
}

#[test]
fn an_empty_scope_has_no_pages_either_way() {
    let store = empty();
    for at in [PageAt::First, PageAt::Last] {
        let page = store
            .thread_page(&query(Scope::Inbox { account_id: 2 }), at)
            .unwrap();
        assert!(page.rows.is_empty() && !page.older && !page.newer);
    }
}

#[test]
fn every_scope_reads_its_own_threads() {
    let mut store = empty();
    put(&mut store, 1, threads("w", 3));
    put(&mut store, 2, threads("h", 2));
    let mut archived = envelope("wa", "twa", BASE_MS + 99 * MINUTE_MS);
    archived.folders = vec!["Label_1".into()];
    let mut labelled = envelope("wl", "twl", BASE_MS + 98 * MINUTE_MS);
    labelled.folders = vec!["INBOX".into(), "Label_1".into()];
    put(&mut store, 1, vec![archived, labelled]);
    let unified = store
        .thread_page(
            &query(Scope::Unified {
                accounts: vec![1, 2],
            }),
            PageAt::First,
        )
        .unwrap();
    assert_eq!(
        unified.rows.len(),
        6,
        "every inbox thread, not the archived one"
    );
    assert!(!subjects(&unified).contains(&"subject wa".to_string()));
    let only_home = store
        .thread_page(&query(Scope::Unified { accounts: vec![2] }), PageAt::First)
        .unwrap();
    assert!(only_home.rows.iter().all(|r| r.account_id == 2));
    let work = store
        .thread_page(&query(Scope::Inbox { account_id: 1 }), PageAt::First)
        .unwrap();
    assert_eq!(work.rows.len(), 4);
    let projects: i64 = store
        .conn()
        .query_row(
            "SELECT id FROM folders WHERE account_id = 1 AND remote_id = 'Label_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let label = store
        .thread_page(
            &query(Scope::Folder {
                account_id: 1,
                folder_id: projects,
            }),
            PageAt::First,
        )
        .unwrap();
    assert_eq!(subjects(&label), ["subject wa", "subject wl"]);
    let nobody = ListQuery {
        account: AccountFilter::Nobody,
        ..query(Scope::Unified {
            accounts: vec![1, 2],
        })
    };
    assert!(
        store
            .thread_page(&nobody, PageAt::First)
            .unwrap()
            .rows
            .is_empty()
    );
    let only = ListQuery {
        account: AccountFilter::Only(2),
        ..query(Scope::Unified {
            accounts: vec![1, 2],
        })
    };
    assert_eq!(
        store.thread_page(&only, PageAt::First).unwrap().rows.len(),
        2
    );
}

/// Dana's unread, starred thread with a PDF on 9 October; Tom's read one on 11 October;
/// a thread whose two messages match different operators.
fn mixed(store: &mut Store, offset: i32) -> (i64, i64) {
    let october_10 = Day::parse("2026-10-10").unwrap().start_ms(offset);
    let mut dana = envelope("m1", "t1", october_10 - 1);
    dana.subject = "Q4 roadmap".into();
    dana.from = vec![Mailbox {
        name: Some("Dana Reyes".into()),
        address: "dana@example.com".into(),
    }];
    dana.to = vec![Mailbox {
        name: None,
        address: "team@corp.example".into(),
    }];
    dana.is_starred = true;
    let mut tom = envelope("m2", "t2", october_10);
    tom.subject = "Lunch".into();
    tom.from = vec![Mailbox {
        name: Some("Tom".into()),
        address: "tom@example.com".into(),
    }];
    tom.cc = vec![Mailbox {
        name: None,
        address: "team@corp.example".into(),
    }];
    tom.is_read = true;
    let mut split_a = envelope("m3", "t3", october_10 + 1);
    split_a.subject = "Split".into();
    split_a.from = vec![Mailbox {
        name: None,
        address: "dana@example.com".into(),
    }];
    split_a.is_read = true;
    let mut split_b = envelope("m4", "t3", october_10 + 2);
    split_b.subject = "Split".into();
    put(store, 1, vec![dana, tom, split_a, split_b]);
    let body = Body {
        text: "the quarterly roadmap".into(),
        attachments: vec![AttachmentMeta {
            part_id: "1".into(),
            filename: "plan.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 2048,
            plain_file: true,
        }],
        ..Body::default()
    };
    let batch = Batch {
        bodies: vec![("m1".into(), body)],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    (october_10 - 1, october_10)
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn every_operator_filters() {
    let mut store = empty();
    mixed(&mut store, 0);
    assert_eq!(matching(&store, "from:dana"), set(&["Q4 roadmap", "Split"]));
    assert_eq!(
        matching(&store, "from:\"Dana Reyes\""),
        set(&["Q4 roadmap"])
    );
    assert_eq!(
        matching(&store, "to:team@corp.example"),
        set(&["Q4 roadmap", "Lunch"]),
        "to: covers cc"
    );
    assert_eq!(matching(&store, "subject:roadmap"), set(&["Q4 roadmap"]));
    assert_eq!(matching(&store, "has:attachment"), set(&["Q4 roadmap"]));
    assert_eq!(matching(&store, "is:unread"), set(&["Q4 roadmap", "Split"]));
    assert_eq!(matching(&store, "is:starred"), set(&["Q4 roadmap"]));
    assert_eq!(
        matching(&store, "quarterly"),
        set(&["Q4 roadmap"]),
        "words search bodies"
    );
    assert_eq!(matching(&store, "account:work"), matching(&store, ""));
    // One message must match every message-level operator: the Split thread's unread message
    // is not Dana's.
    assert_eq!(
        matching(&store, "from:dana is:unread"),
        set(&["Q4 roadmap"])
    );
}

#[test]
fn dates_compare_local_days_at_their_boundaries() {
    for offset in [0, 3_600, -18_000] {
        let mut store = empty();
        mixed(&mut store, offset);
        let at = |text: &str| {
            let q = ListQuery {
                filter: parse(text),
                utc_offset: offset,
                ..query(Scope::Unified {
                    accounts: vec![1, 2],
                })
            };
            subjects(&store.thread_page(&q, PageAt::First).unwrap())
                .into_iter()
                .collect::<BTreeSet<_>>()
        };
        assert_eq!(
            at("before:2026-10-10"),
            set(&["Q4 roadmap"]),
            "offset {offset}"
        );
        assert_eq!(
            at("after:2026-10-10"),
            set(&["Lunch", "Split"]),
            "offset {offset}"
        );
        assert_eq!(
            at("after:2026-10-09 before:2026-10-10"),
            set(&["Q4 roadmap"])
        );
        assert!(at("after:2026-10-11").is_empty());
    }
}

#[test]
fn hostile_strings_in_every_column_and_filter_are_harmless() {
    let mut store = empty();
    let hostile = [
        "100% sure_thing",
        "o'brien \"quoted\" \\back",
        "nul\0byte",
        "esc\u{1b}[2J\u{7}",
        "bidi\u{202e}txt.exe",
        "NEAR(a b) AND * OR -x",
    ];
    let envelopes: Vec<Envelope> = hostile
        .iter()
        .enumerate()
        .map(|(n, text)| {
            let mut e = envelope(&format!("m{n}"), &format!("t{n}"), BASE_MS + n as i64);
            e.subject = (*text).to_string();
            e.snippet = (*text).to_string();
            e.from = vec![Mailbox {
                name: Some((*text).to_string()),
                address: format!("{n}@example.com"),
            }];
            e
        })
        .collect();
    put(&mut store, 1, envelopes);
    let mut huge = envelope("big", "tbig", BASE_MS + 100);
    huge.subject = "x".repeat(1 << 20);
    put(&mut store, 1, vec![huge]);
    assert_eq!(matching(&store, "subject:%"), set(&["100% sure_thing"]));
    assert_eq!(matching(&store, "subject:_"), set(&["100% sure_thing"]));
    assert_eq!(
        matching(&store, "subject:\\"),
        set(&["o'brien \"quoted\" \\back"])
    );
    for filter in [
        "from:'",
        "subject:\"",
        "NEAR(",
        "\"",
        "*",
        "a AND",
        "-x",
        "(",
        "from:%' OR 1=1 --",
        "subject:\0",
        "\u{1b}[2J",
        "from:\u{202e}",
    ] {
        store.thread_page(&filtered(filter), PageAt::First).unwrap();
    }
    let all = store.thread_page(&filtered(""), PageAt::First).unwrap();
    let mut got: BTreeSet<String> = subjects(&all).into_iter().collect();
    got.remove(&"x".repeat(1 << 20));
    assert_eq!(got, hostile.iter().map(|s| (*s).to_string()).collect());
}

#[test]
fn thread_detail_is_oldest_first_with_body_states_links_and_attachments() {
    let mut store = empty();
    let mut first = envelope("m1", "t1", BASE_MS);
    first.is_read = true;
    let second = envelope("m2", "t1", BASE_MS + MINUTE_MS);
    let third = envelope("m3", "t1", BASE_MS + 2 * MINUTE_MS);
    put(&mut store, 1, vec![third, first, second]);
    let fetched = Body {
        text: "see[1]".into(),
        html: Some("<p>see <a href=x>it</a></p>".into()),
        links: vec!["https://a.example".into()],
        attachments: vec![AttachmentMeta {
            part_id: "1".into(),
            filename: "a.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 10,
            plain_file: true,
        }],
        ..Body::default()
    };
    let unreadable = Body {
        text: UNREADABLE_BODY.into(),
        ..Body::default()
    };
    let batch = Batch {
        bodies: vec![("m1".into(), fetched), ("m2".into(), unreadable)],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    let thread: i64 = store
        .conn()
        .query_row("SELECT id FROM threads WHERE remote_id = 't1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    let detail = store.thread_detail(thread).unwrap().unwrap();
    let ids: Vec<&str> = detail
        .messages
        .iter()
        .map(|m| m.remote_id.as_str())
        .collect();
    assert_eq!(ids, ["m1", "m2", "m3"]);
    assert_eq!(detail.older_left_out, 0);
    let m1 = &detail.messages[0];
    assert_eq!(
        m1.body,
        BodyState::Text {
            text: "see[1]".into(),
            cut: false
        }
    );
    assert_eq!(m1.links, ["https://a.example"]);
    assert!(m1.has_html && m1.is_read);
    assert_eq!(m1.attachments[0].filename, "a.pdf");
    assert_eq!(detail.messages[1].body, BodyState::Unreadable);
    assert_eq!(detail.messages[2].body, BodyState::NotFetched);
    assert!(!detail.messages[2].has_html);
    assert_eq!(
        store
            .thread_detail(thread + 99)
            .unwrap()
            .map(|d| d.thread_id),
        None
    );
    let dbg = format!("{:?}", detail);
    assert!(
        !dbg.contains("see[1]") && !dbg.contains("subject m1"),
        "{dbg}"
    );
}

#[test]
fn thread_detail_is_bounded() {
    let mut store = empty();
    let many: Vec<Envelope> = (0..(MAX_THREAD_MESSAGES as i64 + 5))
        .map(|n| envelope(&format!("m{n}"), "t1", BASE_MS + n))
        .collect();
    put(&mut store, 1, many);
    let long = Body {
        text: "y".repeat(MAX_SHOWN_BODY_CHARS + 10),
        ..Body::default()
    };
    let last = format!("m{}", MAX_THREAD_MESSAGES + 4);
    let batch = Batch {
        bodies: vec![(last.clone(), long)],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    let detail = store.thread_detail(1).unwrap().unwrap();
    assert_eq!(detail.messages.len(), MAX_THREAD_MESSAGES);
    assert_eq!(detail.older_left_out, 5);
    assert_eq!(
        detail.messages[0].remote_id, "m5",
        "the newest kept, shown oldest first"
    );
    let BodyState::Text { text, cut } = &detail.messages.last().unwrap().body else {
        panic!("the newest has a body");
    };
    assert!(*cut);
    assert_eq!(text.chars().count(), MAX_SHOWN_BODY_CHARS);
}

#[test]
fn an_unreadable_links_column_reads_as_no_links() {
    let mut store = empty();
    put(&mut store, 1, vec![envelope("m1", "t1", BASE_MS)]);
    store
        .conn()
        .execute(
            "UPDATE messages SET body_text = 'x', body_fetched_at = 1, links = 'not json' WHERE remote_id = 'm1'",
            [],
        )
        .unwrap();
    let detail = store.thread_detail(1).unwrap().unwrap();
    assert!(detail.messages[0].links.is_empty());
}

#[test]
fn message_html_says_whether_it_was_fetched_and_exists() {
    let mut store = empty();
    put(
        &mut store,
        1,
        vec![
            envelope("m1", "t1", BASE_MS),
            envelope("m2", "t1", BASE_MS + 1),
            envelope("m3", "t1", BASE_MS + 2),
        ],
    );
    let html = Body {
        text: "t".into(),
        html: Some("<p>t</p>".into()),
        ..Body::default()
    };
    let plain = Body {
        text: "t".into(),
        ..Body::default()
    };
    let batch = Batch {
        bodies: vec![("m1".into(), html), ("m2".into(), plain)],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    let id = |remote: &str| -> i64 {
        store
            .conn()
            .query_row(
                "SELECT id FROM messages WHERE remote_id = ?1",
                params![remote],
                |r| r.get(0),
            )
            .unwrap()
    };
    assert_eq!(
        store.message_html(id("m1")).unwrap(),
        Some(HtmlBody::Html("<p>t</p>".into()))
    );
    assert_eq!(
        store.message_html(id("m2")).unwrap(),
        Some(HtmlBody::Absent)
    );
    assert_eq!(
        store.message_html(id("m3")).unwrap(),
        Some(HtmlBody::NotFetched)
    );
    assert_eq!(store.message_html(9_999).unwrap(), None);
}

#[test]
fn sidebar_counts_threads_and_unread_per_inbox_and_folder() {
    let mut store = empty();
    let mut read = envelope("m1", "t1", BASE_MS);
    read.is_read = true;
    let unread = envelope("m2", "t2", BASE_MS + 1);
    let mut labelled = envelope("m3", "t3", BASE_MS + 2);
    labelled.folders = vec!["INBOX".into(), "Label_1".into()];
    put(&mut store, 1, vec![read, unread, labelled]);
    let counts = store.sidebar_counts(&[1, 2]).unwrap();
    assert_eq!(
        counts.accounts,
        [
            AccountCounts {
                account_id: 1,
                threads: 3,
                unread: 2
            },
            AccountCounts {
                account_id: 2,
                threads: 0,
                unread: 0
            },
        ]
    );
    let work: Vec<(&str, u64, u64)> = counts
        .folders
        .iter()
        .filter(|f| f.account_id == 1)
        .map(|f| (f.name.as_str(), f.threads, f.unread))
        .collect();
    assert_eq!(work, [("Inbox", 3, 2), ("Sent", 0, 0), ("Projects", 1, 1)]);
    assert_eq!(
        store
            .account_ids(&["W@example.com".into(), "nobody@example.com".into()])
            .unwrap(),
        [Some(1), None]
    );
}

#[test]
fn the_bottom_of_thousands_of_threads_is_quick() {
    let mut store = empty();
    put(&mut store, 1, threads("w", 5_000));
    let q = query(Scope::Unified {
        accounts: vec![1, 2],
    });
    let started = std::time::Instant::now();
    let last = store.thread_page(&q, PageAt::Last).unwrap();
    let up = store
        .thread_page(&q, PageAt::Newer(last.rows[0].key))
        .unwrap();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(up.rows.len(), PAGE);
}

/// The plan lines SQLite gives the page statement of `query` at `at`.
fn plan(store: &Store, query: &ListQuery, at: PageAt) -> String {
    let (base, params) = where_clause(query);
    let (keyset, descending) = direction(at);
    let (sql, values) = rows_statement(&base, &params, keyset, descending);
    let mut stmt = store
        .conn()
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap();
    let rows = stmt
        .query_map(rusqlite::params_from_iter(values), |r| {
            r.get::<_, String>(3)
        })
        .unwrap();
    rows.map(Result::unwrap).collect::<Vec<_>>().join("\n")
}

#[test]
fn the_unified_list_walks_the_date_index_and_never_sorts() {
    let mut store = empty();
    put(&mut store, 1, threads("w", 50));
    let key = ThreadKey {
        last_date: BASE_MS / 1000,
        thread_id: 10,
    };
    let unified = query(Scope::Unified {
        accounts: vec![1, 2],
    });
    for at in [
        PageAt::First,
        PageAt::Last,
        PageAt::Older(key),
        PageAt::Newer(key),
        PageAt::From(key),
    ] {
        let plan = plan(&store, &unified, at);
        assert!(plan.contains("threads_by_date"), "{at:?}: {plan}");
        assert!(!plan.contains("TEMP B-TREE"), "{at:?}: {plan}");
    }
    // The single-account scopes walk the per-account date index; SQLite only orders ties on
    // the last term (the id), never the whole result.
    let inbox = query(Scope::Inbox { account_id: 1 });
    let folder = query(Scope::Folder {
        account_id: 1,
        folder_id: 2,
    });
    for q in [&inbox, &folder] {
        for at in [PageAt::First, PageAt::Last] {
            let plan = plan(&store, q, at);
            assert!(
                plan.contains("threads_by_last_date"),
                "{q:?} {at:?}: {plan}"
            );
            assert!(
                !plan.contains("TEMP B-TREE FOR ORDER BY"),
                "{q:?} {at:?}: {plan}"
            );
        }
    }
}

fn person(name: Option<&str>, address: &str) -> Vec<Mailbox> {
    vec![Mailbox {
        name: name.map(Into::into),
        address: address.into(),
    }]
}

#[test]
fn from_and_to_match_names_and_addresses_never_the_json_around_them() {
    let mut store = empty();
    let mut a = envelope("m1", "t1", BASE_MS);
    a.subject = "Alpha".into();
    a.from = person(Some("Dana Reyes"), "dana@x.example");
    a.to = person(None, "bob@y.example");
    a.cc = person(Some("Carla"), "carla@z.example");
    let mut b = envelope("m2", "t2", BASE_MS + 1);
    b.subject = "Beta".into();
    b.from = person(None, "eve@y.example");
    b.to = person(Some("Dan"), "dan@w.example");
    put(&mut store, 1, vec![a, b]);
    for noise in [
        "from:dre",
        "from:me",
        "from:null",
        "from:address",
        "from:name",
        "from:\\\"",
        "to:null",
        "to:address",
        "to:name",
    ] {
        assert!(matching(&store, noise).is_empty(), "{noise}");
    }
    assert_eq!(
        matching(&store, "from:re"),
        set(&["Alpha"]),
        "Reyes only, not the JSON keys"
    );
    assert_eq!(matching(&store, "from:dana"), set(&["Alpha"]));
    assert_eq!(matching(&store, "from:dana@x"), set(&["Alpha"]));
    assert_eq!(matching(&store, "from:\"dana reyes\""), set(&["Alpha"]));
    assert_eq!(
        matching(&store, "to:a"),
        set(&["Alpha", "Beta"]),
        "carla (cc) and dan, not the JSON"
    );
    assert_eq!(matching(&store, "to:bob"), set(&["Alpha"]));
    assert_eq!(matching(&store, "to:carla"), set(&["Alpha"]), "cc counts");
    assert_eq!(matching(&store, "to:w.example"), set(&["Beta"]));
}
