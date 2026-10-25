//! The two reads bulk selection adds, against a seeded store: a match read a newer press
//! overtakes, and what the `Matched` and `Sender` events put in the log.

use std::time::Duration;

use rail_core::paths::Paths;
use rail_core::store::view::PageAt;

use super::tests::{events, request};
use super::*;
use crate::tui::test_util::{inbox_threads, seed_store};

#[tokio::test]
async fn a_match_read_overtaken_by_a_newer_one_skips_its_query() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let envelopes: Vec<_> = inbox_threads("w", 3).into_iter().map(|e| (0, e)).collect();
    seed_store(&paths, &[("work", "w@example.com")], &envelopes);
    let reader = StoreReader::new(paths.db_file());
    let older = reader.matching_targets(request(1, PageAt::First), 10);
    let newer = reader.matching_targets(request(2, PageAt::First), 10);
    let got = events(older, 1, Duration::from_secs(5)).await;
    assert!(
        matches!(
            got.as_slice(),
            [MailEvent::Matched {
                seq: 1,
                result: Err(ReadError::Stale)
            }]
        ),
        "{got:?}"
    );
    assert_eq!(
        reader.queries(),
        0,
        "the stale read never reached the store"
    );
    let got = events(newer, 1, Duration::from_secs(5)).await;
    let [
        MailEvent::Matched {
            seq: 2,
            result: Ok(targets),
        },
    ] = got.as_slice()
    else {
        panic!("{got:?}")
    };
    assert_eq!(targets.len(), 3);
    assert_eq!(reader.queries(), 1);
}

#[test]
fn the_bulk_events_log_names_and_numbers_only() {
    let sender = MailEvent::Sender {
        seq: 1,
        result: Ok(Some(rail_core::provider::Mailbox {
            name: None,
            address: "dana@example.com".into(),
        })),
    };
    let line = format!("{sender:?}");
    assert!(!line.contains("example.com"), "{line}");
    assert!(!line.contains("Mailbox"), "{line}");
    assert!(
        line.contains("seq: 1") && line.contains("ok: true"),
        "{line}"
    );

    let key = |id: i64| rail_core::store::view::ThreadKey {
        last_date: id,
        thread_id: id,
    };
    let matched = MailEvent::Matched {
        seq: 1,
        result: Ok(vec![
            (key(41), 1, false, true),
            (key(42), 1, true, false),
            (key(43), 2, false, false),
        ]),
    };
    let line = format!("{matched:?}");
    assert!(line.contains("len: 3"), "{line}");
    for id in ["41", "42", "43"] {
        assert!(!line.contains(id), "a thread id reached the log: {line}");
    }

    let failed = MailEvent::Matched {
        seq: 2,
        result: Err(ReadError::NotSetUp),
    };
    let line = format!("{failed:?}");
    assert!(
        line.contains("ok: false") && line.contains("len: 0"),
        "{line}"
    );
}
