//! A snooze on a one-folder mailbox (Microsoft 365): the archive moves the Inbox message to
//! Archive and is recorded when sent; the wake returns exactly that message to the Inbox,
//! leaving an older Archive message, the user's reply in Sent Items and a message now in Deleted
//! Items where they are.

use super::Store;
use super::apply_local_tests::{state, thread};
use super::outbox::{OutboxOutcome, OutboxWrite};
use super::snoozes::SnoozeNew;
use super::snoozes_view_tests::{snoozed, until};
use crate::ops::Action;
use crate::provider::mock::envelope;
use crate::provider::{Envelope, FolderKind, RemoteFolder};
use crate::sync::batch::Batch;

const NOW: i64 = 1_760_000_000;
const HOUR: i64 = 3_600;

fn message(id: &str, ms: i64, folder: &str) -> Envelope {
    Envelope {
        folders: vec![folder.into()],
        ..envelope(id, "c1", ms)
    }
}

fn folders() -> Vec<RemoteFolder> {
    let f = |id: &str, name: &str, kind| RemoteFolder {
        parent: None,
        remote_id: id.into(),
        name: name.into(),
        kind,
    };
    vec![
        f("f-inbox", "Inbox", FolderKind::Inbox),
        f("f-sent", "Sent Items", FolderKind::Sent),
        f("f-arch", "Archive", FolderKind::Archive),
        f("f-del", "Deleted Items", FolderKind::Trash),
    ]
}

/// A Microsoft 365 account (id 1) whose conversation c1 holds m-old (filed in Archive before
/// the snooze), m-in and m-in2 (Inbox) and m-sent (the user's reply in Sent Items).
fn seeded() -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('corp', 'm365', 'me@corp.example')",
            [],
        )
        .unwrap();
    let batch = Batch {
        folders: Some(folders()),
        upserts: vec![
            message("m-old", 1_000, "f-arch"),
            message("m-in", 2_000, "f-inbox"),
            message("m-sent", 3_000, "f-sent"),
            message("m-in2", 4_000, "f-inbox"),
        ],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    store
}

/// Each message's folders, in remote-id order: m-in, m-in2, m-old, m-sent.
fn places(store: &Store) -> Vec<(String, Vec<String>)> {
    state(store).into_iter().map(|m| (m.0, m.3)).collect()
}

fn place(id: &str, folder: &str) -> (String, Vec<String>) {
    (id.to_string(), vec![folder.to_string()])
}

fn write(store: &mut Store, write: &OutboxWrite, at: i64) -> OutboxOutcome {
    store.outbox_write(write, at).unwrap()
}

fn send_next(store: &mut Store, at: i64) {
    let OutboxOutcome::Claimed(Some(row)) = write(store, &OutboxWrite::Claim { account_id: 1 }, at)
    else {
        panic!("no row to send");
    };
    let done = OutboxWrite::Complete {
        id: row.id,
        draft_consumed: false,
    };
    write(store, &done, at);
}

#[test]
fn a_snooze_returns_exactly_the_archived_inbox_messages_to_the_inbox() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let snooze = OutboxWrite::Snooze(SnoozeNew {
        account_id: 1,
        threads: vec![c1],
        until: NOW + HOUR,
        not_before: NOW + 10,
        client_token: "a".into(),
    });
    write(&mut store, &snooze, NOW);
    assert_eq!(
        places(&store),
        vec![
            place("m-in", "f-arch"),
            place("m-in2", "f-arch"),
            place("m-old", "f-arch"),
            place("m-sent", "f-sent"),
        ]
    );
    send_next(&mut store, NOW + 10);
    assert_eq!(
        store.snoozes(1).unwrap()[0].messages,
        vec!["m-in".to_string(), "m-in2".to_string()]
    );
    let deleted = Batch {
        upserts: vec![message("m-in2", 4_000, "f-del")],
        ..Batch::new(1)
    };
    store.apply(&deleted, NOW + 60).unwrap();
    let OutboxOutcome::Woke { threads } =
        write(&mut store, &OutboxWrite::Wake { account_id: 1 }, NOW + HOUR)
    else {
        panic!("not woken");
    };
    assert_eq!(threads, vec![c1]);
    let queued: Vec<Action> = store
        .pending_for(&[c1])
        .unwrap()
        .into_iter()
        .map(|r| r.action)
        .collect();
    assert_eq!(
        queued,
        vec![
            Action::Unsnooze {
                messages: vec!["m-in".into()]
            },
            Action::Read { on: false },
        ]
    );
    assert_eq!(
        places(&store),
        vec![
            place("m-in", "f-inbox"),
            place("m-in2", "f-del"),
            place("m-old", "f-arch"),
            place("m-sent", "f-sent"),
        ]
    );
}

#[test]
fn a_snooze_holds_its_conversation_only_while_some_of_its_mail_is_in_archive() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let snooze = OutboxWrite::Snooze(SnoozeNew {
        account_id: 1,
        threads: vec![c1],
        until: NOW + HOUR,
        not_before: NOW + 10,
        client_token: "a".into(),
    });
    write(&mut store, &snooze, NOW);
    send_next(&mut store, NOW + 10);
    let deleted = |id: &str, ms| Batch {
        upserts: vec![message(id, ms, "f-del")],
        ..Batch::new(1)
    };
    store.apply(&deleted("m-in", 2_000), NOW + 20).unwrap();
    assert_eq!(snoozed(&store), [c1], "m-in2 is still in Archive");
    // m-old was in Archive before the snooze, so it does not hold the conversation.
    store.apply(&deleted("m-in2", 4_000), NOW + 30).unwrap();
    assert_eq!(snoozed(&store), Vec::<i64>::new());
    assert_eq!(until(&store, c1), None);
}
