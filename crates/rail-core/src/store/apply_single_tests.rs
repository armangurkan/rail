//! The optimistic change on a one-folder mailbox (Microsoft 365): archive, trash and move put a
//! picked message in the target alone and leave Sent Items where it is; star and read touch
//! the picked messages only; undo, a permanent failure and a delta restore or hold exactly;
//! a move into the folder every message is in changes nothing; and a row's targets show each
//! message as it was before the row.

use super::Store;
use super::apply_local_tests::{enqueue, folder, state, thread, undo};
use super::outbox::{Failure, OutboxOutcome, OutboxWrite};
use crate::actions::scope::touched;
use crate::config::FolderModel;
use crate::ops::Action;
use crate::provider::mock::envelope;
use crate::provider::{Change, Envelope, FolderKind, RemoteFolder, TargetMessage};
use crate::sync::batch::Batch;

const NOW: i64 = 1_760_000_000;

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
        f("f-proj", "Projects", FolderKind::User),
    ]
}

fn message(id: &str, thread: &str, ms: i64, folder: &str, read: bool) -> Envelope {
    Envelope {
        folders: vec![folder.into()],
        is_read: read,
        ..envelope(id, thread, ms)
    }
}

/// A Microsoft 365 account (id 1): conversation c1 holds m1 (Inbox, unread), m2 (the user's
/// reply in Sent Items) and m3 (Projects, unread, the newest); c2 holds m4 (Projects).
fn seeded() -> Store {
    seeded_with(folders())
}

/// `seeded`, with the account's folders listed (and so numbered) in `folders`' order.
fn seeded_with(folders: Vec<RemoteFolder>) -> Store {
    let mut store = Store::open_in_memory().unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('corp', 'm365', 'me@corp.example')",
            [],
        )
        .unwrap();
    let batch = Batch {
        folders: Some(folders),
        upserts: vec![
            message("m1", "c1", 1_000, "f-inbox", false),
            message("m2", "c1", 2_000, "f-sent", true),
            message("m3", "c1", 3_000, "f-proj", false),
            message("m4", "c2", 4_000, "f-proj", true),
        ],
        ..Batch::new(1)
    };
    store.apply(&batch, NOW).unwrap();
    store
}

/// Each message's folders, in `state` order (m1, m2, m3, m4).
fn places(store: &Store) -> Vec<Vec<String>> {
    state(store).into_iter().map(|m| m.3).collect()
}

fn claim(store: &mut Store) -> super::outbox::OutboxRow {
    match store
        .outbox_write(&OutboxWrite::Claim { account_id: 1 }, NOW + 20)
        .unwrap()
    {
        OutboxOutcome::Claimed(Some(row)) => row,
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_account_reads_as_a_one_folder_mailbox() {
    assert_eq!(seeded().folder_model(1).unwrap(), FolderModel::SingleFolder);
}

#[test]
fn archive_moves_only_the_inbox_messages_and_undo_puts_them_back() {
    let mut store = seeded();
    let before = state(&store);
    let c1 = thread(&store, "c1");
    let id = enqueue(&mut store, Action::Archive {}, vec![c1], "a");
    assert_eq!(
        places(&store),
        [
            vec!["f-arch"],
            vec!["f-sent"],
            vec!["f-proj"],
            vec!["f-proj"]
        ]
    );
    undo(&mut store, vec![id]);
    assert_eq!(state(&store), before);
}

#[test]
fn trash_moves_every_message_but_sent_items_to_deleted_items() {
    let mut store = seeded();
    let before = state(&store);
    let c1 = thread(&store, "c1");
    let id = enqueue(&mut store, Action::Trash {}, vec![c1], "a");
    assert_eq!(
        places(&store),
        [vec!["f-del"], vec!["f-sent"], vec!["f-del"], vec!["f-proj"]]
    );
    undo(&mut store, vec![id]);
    assert_eq!(state(&store), before);
}

#[test]
fn a_move_skips_sent_items_and_messages_already_there() {
    let mut store = seeded();
    let before = state(&store);
    let c1 = thread(&store, "c1");
    let to = folder(&store, "f-proj");
    let id = enqueue(&mut store, Action::Move { to }, vec![c1], "a");
    assert_eq!(
        places(&store),
        [
            vec!["f-proj"],
            vec!["f-sent"],
            vec!["f-proj"],
            vec!["f-proj"]
        ]
    );
    undo(&mut store, vec![id]);
    assert_eq!(state(&store), before);
}

#[test]
fn a_move_into_the_folder_every_message_is_in_changes_nothing() {
    let mut store = seeded();
    let before = state(&store);
    let c2 = thread(&store, "c2");
    let to = folder(&store, "f-proj");
    let id = enqueue(&mut store, Action::Move { to }, vec![c2], "a");
    assert_eq!(state(&store), before);
    let reversal: String = store
        .conn()
        .query_row("SELECT reversal FROM outbox WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(reversal, "[]", "nothing to put back");
    assert!(matches!(
        undo(&mut store, vec![id]),
        OutboxOutcome::Undone { .. }
    ));
    assert_eq!(state(&store), before);
}

#[test]
fn read_and_star_set_the_flag_on_the_picked_messages_only() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let flags = |store: &Store, column: usize| -> Vec<String> {
        state(store)[..3]
            .iter()
            .filter(|m| if column == 1 { !m.1 } else { m.2 })
            .map(|m| m.0.clone())
            .collect()
    };
    enqueue(&mut store, Action::Read { on: true }, vec![c1], "a");
    assert!(flags(&store, 1).is_empty(), "every unread message read");
    enqueue(&mut store, Action::Read { on: false }, vec![c1], "b");
    assert_eq!(flags(&store, 1), ["m3"], "only the newest is unread");
    enqueue(&mut store, Action::Star { on: true }, vec![c1], "c");
    assert_eq!(flags(&store, 2), ["m3"], "only the newest is flagged");
    enqueue(&mut store, Action::Star { on: true }, vec![c1], "d");
    assert_eq!(
        flags(&store, 2),
        ["m3"],
        "a flagged thread flags nothing more"
    );
    store
        .conn()
        .execute(
            "UPDATE messages SET is_starred = 1 WHERE remote_id = 'm1'",
            [],
        )
        .unwrap();
    enqueue(&mut store, Action::Star { on: false }, vec![c1], "e");
    assert!(flags(&store, 2).is_empty(), "unstar clears every flag");
}

#[test]
fn a_permanent_failure_of_a_move_puts_the_messages_back() {
    let mut store = seeded();
    let before = state(&store);
    let c1 = thread(&store, "c1");
    let to = folder(&store, "f-arch");
    let id = enqueue(&mut store, Action::Move { to }, vec![c1], "a");
    claim(&mut store);
    let fail = OutboxWrite::Fail {
        id,
        failure: Failure::Permanent("Microsoft 365 refused the request (HTTP 400)".into()),
    };
    store.outbox_write(&fail, NOW + 20).unwrap();
    assert_eq!(state(&store), before);
}

#[test]
fn a_delta_still_showing_the_old_folder_does_not_undo_a_pending_archive() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let id = enqueue(&mut store, Action::Archive {}, vec![c1], "a");
    let old = Batch {
        upserts: vec![message("m1", "c1", 1_000, "f-inbox", false)],
        ..Batch::new(1)
    };
    store.apply(&old, NOW + 1).unwrap();
    assert_eq!(places(&store)[0], ["f-arch"], "the pending archive holds");
    undo(&mut store, vec![id]);
    assert_eq!(places(&store)[0], ["f-inbox"], "back in the Inbox alone");
}

#[test]
fn a_delta_that_already_shows_the_move_keeps_it_and_undo_is_too_late() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let id = enqueue(&mut store, Action::Archive {}, vec![c1], "a");
    claim(&mut store);
    let moved = Batch {
        upserts: vec![message("m1", "c1", 1_000, "f-arch", false)],
        ..Batch::new(1)
    };
    store.apply(&moved, NOW + 21).unwrap();
    assert_eq!(places(&store)[0], ["f-arch"]);
    let outcome = undo(&mut store, vec![id]);
    assert!(
        matches!(&outcome, OutboxOutcome::Undone { too_late, .. } if too_late == &[id]),
        "{outcome:?}"
    );
    assert_eq!(places(&store)[0], ["f-arch"], "undo changed nothing");
    store
        .outbox_write(&OutboxWrite::Complete { id }, NOW + 22)
        .unwrap();
    assert_eq!(places(&store)[0], ["f-arch"], "what Outlook said stands");
}

#[test]
fn a_rows_targets_show_each_message_as_it_was_before_the_row() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let archive = enqueue(&mut store, Action::Archive {}, vec![c1], "a");
    enqueue(&mut store, Action::Read { on: true }, vec![c1], "b");
    let now = store.thread_targets(1, &[c1]).unwrap();
    assert_eq!(now[0].messages[0].folder, Some(FolderKind::Archive));
    assert!(!now[0].messages[0].unread, "the store shows both changes");
    let row = claim(&mut store);
    assert_eq!(row.id, archive);
    let before = store.row_targets(&row).unwrap();
    let m1 = &before[0].messages[0];
    assert_eq!(
        (m1.folder, m1.folder_remote.as_deref()),
        (Some(FolderKind::Inbox), Some("f-inbox")),
        "the archive row sees m1 where it was"
    );
    store
        .outbox_write(&OutboxWrite::Complete { id: archive }, NOW + 20)
        .unwrap();
    let read = claim(&mut store);
    let facts: Vec<TargetMessage> = store.row_targets(&read).unwrap().remove(0).messages;
    let unread: Vec<&str> = facts
        .iter()
        .filter(|m| m.unread)
        .map(|m| m.id.as_str())
        .collect();
    assert_eq!(unread, ["m1", "m3"], "the read row sees what was unread");
}

/// A pending archive of c1, then a delta that shows another client moved m1 to Projects.
fn archived_then_moved_elsewhere() -> (Store, i64) {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let id = enqueue(&mut store, Action::Archive {}, vec![c1], "a");
    let moved = Batch {
        upserts: vec![message("m1", "c1", 1_000, "f-proj", false)],
        ..Batch::new(1)
    };
    store.apply(&moved, NOW + 1).unwrap();
    assert_eq!(places(&store)[0], ["f-proj"], "the server's folder stands");
    (store, id)
}

#[test]
fn undo_after_a_delta_moved_the_message_elsewhere_leaves_it_where_the_server_put_it() {
    let (mut store, id) = archived_then_moved_elsewhere();
    undo(&mut store, vec![id]);
    assert_eq!(places(&store)[0], ["f-proj"], "in exactly one folder");
}

#[test]
fn a_permanent_failure_after_a_delta_moved_the_message_elsewhere_keeps_the_server_folder() {
    let (mut store, id) = archived_then_moved_elsewhere();
    claim(&mut store);
    let fail = OutboxWrite::Fail {
        id,
        failure: Failure::Permanent("Microsoft 365 refused the request (HTTP 400)".into()),
    };
    store.outbox_write(&fail, NOW + 20).unwrap();
    assert_eq!(places(&store)[0], ["f-proj"], "in exactly one folder");
}

#[test]
fn undo_with_no_delta_still_puts_the_message_back_in_the_inbox() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let id = enqueue(&mut store, Action::Archive {}, vec![c1], "a");
    undo(&mut store, vec![id]);
    assert_eq!(places(&store)[0], ["f-inbox"]);
}

/// A pending move of c1 to Projects (only m1 moves), then a folder sync that no longer lists
/// Projects: its row goes, and with it m1's only folder.
fn moved_then_target_deleted() -> (Store, i64) {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let to = folder(&store, "f-proj");
    let id = enqueue(&mut store, Action::Move { to }, vec![c1], "a");
    assert_eq!(places(&store)[0], ["f-proj"]);
    let listing = Batch {
        folders: Some(
            folders()
                .into_iter()
                .filter(|f| f.remote_id != "f-proj")
                .collect(),
        ),
        ..Batch::new(1)
    };
    store.apply(&listing, NOW + 1).unwrap();
    assert!(places(&store)[0].is_empty(), "m1's folder row is gone");
    (store, id)
}

fn fail_for_good(store: &mut Store, id: i64) {
    claim(store);
    let fail = OutboxWrite::Fail {
        id,
        failure: Failure::Permanent("the folder for this move no longer exists".into()),
    };
    store.outbox_write(&fail, NOW + 20).unwrap();
}

#[test]
fn a_failed_move_whose_folder_is_gone_puts_the_message_back_in_the_inbox() {
    let (mut store, id) = moved_then_target_deleted();
    fail_for_good(&mut store, id);
    assert_eq!(places(&store)[0], ["f-inbox"], "in exactly its old folder");
}

#[test]
fn undo_of_a_move_whose_folder_is_gone_puts_the_message_back_in_the_inbox() {
    let (mut store, id) = moved_then_target_deleted();
    undo(&mut store, vec![id]);
    assert_eq!(places(&store)[0], ["f-inbox"], "in exactly its old folder");
}

#[test]
fn a_sync_after_the_move_folder_is_gone_keeps_the_way_back_to_the_inbox() {
    let (mut store, id) = moved_then_target_deleted();
    let delta = Batch {
        upserts: vec![message("m2", "c1", 2_000, "f-sent", true)],
        ..Batch::new(1)
    };
    store.apply(&delta, NOW + 2).unwrap();
    fail_for_good(&mut store, id);
    assert_eq!(places(&store)[0], ["f-inbox"], "in exactly its old folder");
}

/// The folders as Graph lists them, by name: Archive and Deleted Items number below Inbox.
fn folders_by_name() -> Vec<RemoteFolder> {
    let mut by_name = folders();
    by_name.sort_by(|a, b| a.name.cmp(&b.name));
    by_name
}

#[test]
fn a_stacked_rows_targets_show_the_exact_folder_each_message_left() {
    let mut store = seeded_with(folders_by_name());
    let c1 = thread(&store, "c1");
    let archive = enqueue(&mut store, Action::Archive {}, vec![c1], "a");
    enqueue(&mut store, Action::Trash {}, vec![c1], "b");
    assert_eq!(places(&store)[0], ["f-del"], "the store shows both rows");
    let row = claim(&mut store);
    assert_eq!(row.id, archive);
    let m1 = store
        .row_targets(&row)
        .unwrap()
        .remove(0)
        .messages
        .remove(0);
    assert_eq!(
        (m1.folder, m1.folder_remote.as_deref()),
        (Some(FolderKind::Inbox), Some("f-inbox")),
        "the archive row sees m1 in the Inbox"
    );
    store
        .outbox_write(&OutboxWrite::Complete { id: archive }, NOW + 20)
        .unwrap();
    let trash = claim(&mut store);
    let m1 = store
        .row_targets(&trash)
        .unwrap()
        .remove(0)
        .messages
        .remove(0);
    assert_eq!(
        (m1.folder, m1.folder_remote.as_deref()),
        (Some(FolderKind::Archive), Some("f-arch")),
        "the trash row sees m1 in Archive"
    );
}

#[test]
fn a_stacked_archive_is_sent_for_the_inbox_message_when_the_trash_after_it_fails() {
    let mut store = seeded_with(folders_by_name());
    let c1 = thread(&store, "c1");
    let archive = enqueue(&mut store, Action::Archive {}, vec![c1], "a");
    let trash = enqueue(&mut store, Action::Trash {}, vec![c1], "b");
    let row = claim(&mut store);
    let facts = store.row_targets(&row).unwrap().remove(0).messages;
    assert_eq!(
        touched(&Change::Archive, &facts),
        [0],
        "the archive request names m1"
    );
    store
        .outbox_write(&OutboxWrite::Complete { id: archive }, NOW + 20)
        .unwrap();
    fail_for_good(&mut store, trash);
    assert_eq!(
        places(&store)[..3],
        [vec!["f-arch"], vec!["f-sent"], vec!["f-proj"]],
        "the store shows what Outlook did: m1 archived, the trash undone"
    );
}
