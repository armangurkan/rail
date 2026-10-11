//! Migration 0005 and the writer: senders and recipients reach the full-text index as plain
//! text (`Name address`), never as the JSON around them.

use std::path::{Path, PathBuf};

use rusqlite::params;

use super::sync_write::address_text;
use super::{Store, fts_query, migrations};
use crate::provider::Mailbox;
use crate::provider::mock::envelope;
use crate::sync::batch::Batch;

const NOW: i64 = 1_760_100_000;

fn mailbox(name: Option<&str>, address: &str) -> Mailbox {
    Mailbox {
        name: name.map(Into::into),
        address: address.into(),
    }
}

/// The JSON the writer stores for a list (the same `serde_json` call).
fn json(mailboxes: &[Mailbox]) -> String {
    serde_json::to_string(mailboxes).unwrap()
}

/// Message ids whose index entry matches `word`, quoted as the filter and `rail search` do.
fn hits(store: &Store, word: &str) -> Vec<i64> {
    let query = fts_query(word).unwrap();
    let mut stmt = store
        .conn()
        .prepare("SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1 ORDER BY rowid")
        .unwrap();
    stmt.query_map(params![query], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// A version-4 store at `dir/rail.db` (the last build's schema) with account 1 and thread 1.
fn version_4(dir: &Path) -> (PathBuf, Store) {
    let path = dir.join("rail.db");
    let old = Store::open_at_version(&path, 4).unwrap();
    assert_eq!(old.schema_version().unwrap(), 4);
    old.conn()
        .execute_batch(
            "INSERT INTO accounts (name, kind, address) VALUES ('a', 'gmail', 'a@example.com');
             INSERT INTO threads (account_id, remote_id) VALUES (1, 't1');",
        )
        .unwrap();
    (path, old)
}

/// A message row as version 4 wrote it, with these raw `from_json`, `to_json`, `cc_json` and
/// `bcc_json` values; its id.
fn old_message(store: &Store, remote: &str, subject: &str, columns: [&str; 4]) -> i64 {
    let [from, to, cc, bcc] = columns;
    store
        .conn()
        .execute(
            "INSERT INTO messages (account_id, thread_id, remote_id, subject, from_json, to_json,
               cc_json, bcc_json, internal_date)
             VALUES (1, 1, ?1, ?2, ?3, ?4, ?5, ?6, 1000)",
            params![remote, subject, from, to, cc, bcc],
        )
        .unwrap();
    store.conn().last_insert_rowid()
}

fn texts(store: &Store, remote: &str) -> (String, String) {
    store
        .conn()
        .query_row(
            "SELECT from_text, to_text FROM messages WHERE remote_id = ?1",
            params![remote],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap()
}

fn integrity_check(store: &Store) {
    store
        .conn()
        .execute(
            "INSERT INTO messages_fts(messages_fts, rank) VALUES ('integrity-check', 1)",
            [],
        )
        .unwrap();
}

#[test]
fn address_text_pairs_each_name_with_its_address() {
    assert_eq!(address_text(std::iter::empty()), "");
    assert_eq!(
        address_text(&[
            mailbox(Some("Dana Reyes"), "dana@x.example"),
            mailbox(None, "bob@y.example"),
            mailbox(Some(""), "e@z.example"),
        ]),
        "Dana Reyes dana@x.example bob@y.example e@z.example"
    );
}

/// The SQL backfill and `address_text` must agree byte for byte: every case is written as
/// version-4 JSON by `serde_json` (as the old writer did), migrated by 0005's SQL, and the
/// stored text compared with what `address_text` gives for the same lists.
#[test]
fn the_upgrade_backfill_gives_the_writers_text_for_hostile_names() {
    let none = Vec::new;
    let cases: Vec<[Vec<Mailbox>; 4]> = vec![
        [none(), none(), none(), none()],
        [
            vec![mailbox(None, "plain@x.example")],
            vec![mailbox(Some(""), "empty@x.example")],
            none(),
            none(),
        ],
        [
            vec![
                mailbox(Some("Reyes, Dana \"DR\""), "dana@x.example"),
                mailbox(None, "second@x.example"),
            ],
            vec![mailbox(Some("To One"), "t1@x.example")],
            vec![mailbox(Some("Cee"), "cc@x.example")],
            vec![mailbox(Some("Bee"), "bcc@x.example")],
        ],
        [
            vec![mailbox(
                Some("tab\tline\nbell\u{7}esc\u{1b}[31m nul\u{0}end"),
                "ctl@x.example",
            )],
            none(),
            none(),
            none(),
        ],
        [
            vec![mailbox(Some("Zoë 山田 \u{202e}evil"), "zoe@例え.jp")],
            none(),
            none(),
            none(),
        ],
        [
            vec![mailbox(Some("  spaced  "), "sp@x.example")],
            vec![mailbox(Some("{\"name\":null}"), "json@x.example")],
            none(),
            none(),
        ],
    ];
    let dir = tempfile::tempdir().unwrap();
    let (path, old) = version_4(dir.path());
    for (n, case) in cases.iter().enumerate() {
        let [from, to, cc, bcc] = case.each_ref().map(|list| json(list.as_slice()));
        old_message(&old, &format!("m{n}"), "s", [&from, &to, &cc, &bcc]);
    }
    // A tampered row: unreadable JSON and entries that are not objects contribute nothing,
    // and do not stop the upgrade.
    old_message(
        &old,
        "tampered",
        "s",
        ["not json", "[\"x\", 3]", "{}", "[]"],
    );
    drop(old);

    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), migrations::LATEST_VERSION);
    for (n, [from, to, cc, bcc]) in cases.iter().enumerate() {
        let (from_text, to_text) = texts(&store, &format!("m{n}"));
        assert_eq!(from_text, address_text(from), "case {n}: from");
        assert_eq!(
            to_text,
            address_text(to.iter().chain(cc).chain(bcc)),
            "case {n}: to, cc, bcc"
        );
    }
    assert_eq!(texts(&store, "tampered"), (String::new(), String::new()));
    integrity_check(&store);
}

#[test]
fn after_the_upgrade_json_keys_match_nothing_and_people_still_match() {
    let dir = tempfile::tempdir().unwrap();
    let (path, old) = version_4(dir.path());
    let dana = old_message(
        &old,
        "m1",
        "Q4 roadmap",
        [
            &json(&[mailbox(Some("Dana Reyes"), "dana@x.example")]),
            &json(&[mailbox(None, "bob@y.example")]),
            "[]",
            "[]",
        ],
    );
    let named = old_message(
        &old,
        "m2",
        "what is in a name",
        [&json(&[mailbox(None, "eve@w.example")]), "[]", "[]", "[]"],
    );
    assert_eq!(
        hits(&old, "null"),
        [dana, named],
        "version 4 indexes the JSON"
    );
    drop(old);

    let store = Store::open(&path).unwrap();
    assert_eq!(
        hits(&store, "name"),
        [named],
        "only the subject that says it"
    );
    assert!(hits(&store, "address").is_empty());
    assert!(hits(&store, "null").is_empty());
    assert_eq!(hits(&store, "dana"), [dana]);
    assert_eq!(hits(&store, "Reyes"), [dana]);
    assert_eq!(hits(&store, "dana@x.example"), [dana]);
    assert_eq!(hits(&store, "bob"), [dana], "recipients by address");
    assert_eq!(hits(&store, "eve"), [named]);
    integrity_check(&store);
}

#[test]
fn mail_written_after_the_upgrade_is_indexed_the_same_way() {
    let dir = tempfile::tempdir().unwrap();
    let (path, old) = version_4(dir.path());
    drop(old);
    let mut store = Store::open(&path).unwrap();
    let mut env = envelope("m9", "t9", 1_760_000_000_000);
    env.folders = Vec::new();
    env.from = vec![mailbox(Some("Dana Reyes"), "dana@x.example")];
    env.to = vec![mailbox(None, "bob@y.example")];
    env.cc = vec![mailbox(Some("Carla"), "carla@z.example")];
    env.bcc = vec![mailbox(Some("Hidden"), "h@z.example")];
    let batch = Batch {
        upserts: vec![env.clone()],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    assert_eq!(
        texts(&store, "m9"),
        (
            address_text(&env.from),
            address_text(env.to.iter().chain(&env.cc).chain(&env.bcc))
        )
    );
    let id: i64 = store
        .conn()
        .query_row("SELECT id FROM messages WHERE remote_id = 'm9'", [], |r| {
            r.get(0)
        })
        .unwrap();
    for word in ["dana", "bob", "carla", "hidden"] {
        assert_eq!(hits(&store, word), [id], "{word}");
    }
    for word in ["name", "address", "null"] {
        assert!(hits(&store, word).is_empty(), "{word}");
    }
    // A re-sync that changes the sender re-indexes through the update trigger.
    env.from = vec![mailbox(Some("Tom"), "tom@x.example")];
    let batch = Batch {
        upserts: vec![env],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    assert!(hits(&store, "dana").is_empty());
    assert_eq!(hits(&store, "tom"), [id]);
    integrity_check(&store);
}

#[test]
fn the_upgrade_runs_on_an_empty_store() {
    let dir = tempfile::tempdir().unwrap();
    let (path, old) = version_4(dir.path());
    drop(old);
    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), migrations::LATEST_VERSION);
    let sql: String = store
        .conn()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'messages_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(
        sql.contains("subject, from_text, to_text, body_text"),
        "{sql}"
    );
    assert!(hits(&store, "anything").is_empty());
    integrity_check(&store);
}

#[test]
fn the_upgrade_survives_a_version_4_store_missing_a_trigger() {
    let dir = tempfile::tempdir().unwrap();
    let (path, old) = version_4(dir.path());
    let id = old_message(
        &old,
        "m1",
        "hello",
        [&json(&[mailbox(None, "dana@x.example")]), "[]", "[]", "[]"],
    );
    old.conn()
        .execute_batch("DROP TRIGGER messages_fts_update;")
        .unwrap();
    drop(old);

    let store = Store::open(&path).unwrap();
    assert_eq!(store.schema_version().unwrap(), migrations::LATEST_VERSION);
    let triggers: i64 = store
        .conn()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'trigger' AND name LIKE 'messages_fts_%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(triggers, 3, "all three triggers exist again");
    assert_eq!(hits(&store, "dana"), [id]);
    integrity_check(&store);
}
