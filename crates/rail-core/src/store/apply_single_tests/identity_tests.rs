use super::*;
use rusqlite::params;

/// A folder's old row now belongs to another account (with the same provider id), and the
/// original account gets that provider folder under a new id. Optionally move memberships.
fn renumber(store: &Store, remote: &str, memberships: bool) {
    let old = folder(store, remote);
    let conn = store.conn();
    conn.execute(
        "INSERT INTO accounts (name, kind, address) VALUES ('other', 'm365', 'other@example.com')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE folders SET account_id = 2, kind = 'sent' WHERE id = ?1",
        [old],
    )
    .unwrap();
    let kind = if remote == "f-inbox" {
        "inbox"
    } else {
        "archive"
    };
    conn.execute(
        "INSERT INTO folders (account_id, remote_id, name, kind) VALUES (1, ?1, 'Restored', ?2)",
        params![remote, kind],
    )
    .unwrap();
    let new = conn.last_insert_rowid();
    if memberships {
        conn.execute(
            "UPDATE message_folders SET folder_id = ?2 WHERE folder_id = ?1",
            params![old, new],
        )
        .unwrap();
    }
}

fn first_facts(store: &Store, row: i64) -> TargetMessage {
    store
        .row_targets(&store.outbox_row(row).unwrap().unwrap())
        .unwrap()
        .remove(0)
        .messages
        .remove(0)
}

#[test]
fn stacked_provider_targets_resolve_the_original_folder_in_the_right_account() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let archive = enqueue(&mut store, Action::Archive {}, vec![c1], "archive");
    let read = enqueue(&mut store, Action::Read { on: true }, vec![c1], "read");
    renumber(&store, "f-inbox", false);
    let facts = first_facts(&store, archive);
    assert_eq!(facts.folder, Some(FolderKind::Inbox));
    assert_eq!(facts.folder_remote.as_deref(), Some("f-inbox"));
    assert_eq!(touched(&Change::Archive, &[facts]), [0]);
    undo(&mut store, vec![read]);
    assert_eq!(first_facts(&store, archive).folder, Some(FolderKind::Inbox));
}

#[test]
fn stacked_undo_recognizes_the_added_folder_after_it_gets_a_new_id() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let before = places(&store);
    let archive = enqueue(&mut store, Action::Archive {}, vec![c1], "archive");
    let read = enqueue(&mut store, Action::Read { on: true }, vec![c1], "read");
    renumber(&store, "f-arch", true);
    undo(&mut store, vec![read]);
    undo(&mut store, vec![archive]);
    assert_eq!(
        places(&store),
        before,
        "the same Archive folder is still the action's target"
    );
}

#[test]
fn stacked_undo_does_not_mistake_a_reused_added_id_for_its_original_folder() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let archive = enqueue(&mut store, Action::Archive {}, vec![c1], "archive");
    let read = enqueue(&mut store, Action::Read { on: true }, vec![c1], "read");
    store.conn().execute_batch("UPDATE folders SET remote_id = 'f-other', kind = 'user' WHERE remote_id = 'f-arch';
        INSERT INTO folders (account_id, remote_id, name, kind) VALUES (1, 'f-arch', 'Archive', 'archive');").unwrap();
    undo(&mut store, vec![read]);
    assert_eq!(places(&store)[0], ["f-other"], "the server's move stands");
    undo(&mut store, vec![archive]);
    assert_eq!(places(&store)[0], ["f-other"]);
}

#[test]
fn provider_targets_do_not_use_a_different_folder_that_reuses_the_removed_id() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let archive = enqueue(&mut store, Action::Archive {}, vec![c1], "archive");
    store
        .conn()
        .execute(
            "UPDATE folders SET remote_id = 'f-other', kind = 'sent' WHERE remote_id = 'f-inbox'",
            [],
        )
        .unwrap();
    let facts = first_facts(&store, archive);
    assert_eq!((facts.folder, facts.folder_remote), (None, None));
}

#[test]
fn legacy_provider_targets_still_stay_in_the_rows_account() {
    let mut store = seeded();
    let c1 = thread(&store, "c1");
    let archive = enqueue(&mut store, Action::Archive {}, vec![c1], "archive");
    store.conn().execute("UPDATE outbox SET reversal = (SELECT json_group_array(json(json_remove(value, '$.remotes'))) FROM json_each(outbox.reversal)) WHERE id = ?1", [archive]).unwrap();
    renumber(&store, "f-inbox", false);
    let facts = first_facts(&store, archive);
    assert_eq!((facts.folder, facts.folder_remote), (None, None));
}
