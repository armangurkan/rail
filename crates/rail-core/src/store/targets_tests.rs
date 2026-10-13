//! `Store::thread_targets`: the threads a row names that the store holds, in store order, each
//! with its messages' facts (folder kind and id, flags, date); another account's threads are
//! not this one's.

use super::apply_local_tests::{folder, seeded, thread};
use crate::provider::{FolderKind, TargetMessage, ThreadTarget};

fn fact(
    id: &str,
    folder: Option<(FolderKind, &str)>,
    unread: bool,
    starred: bool,
    date: i64,
) -> TargetMessage {
    TargetMessage {
        id: id.into(),
        folder: folder.map(|(kind, _)| kind),
        folder_remote: folder.map(|(_, remote)| remote.to_string()),
        unread,
        starred,
        date,
    }
}

#[test]
fn thread_targets_carry_each_messages_folder_flags_and_date() {
    let store = seeded();
    let (t1, t2) = (thread(&store, "t1"), thread(&store, "t2"));
    let label = folder(&store, "Label_1");
    store
        .conn()
        .execute_batch(&format!(
            "UPDATE messages SET is_read = 1 WHERE remote_id = 'm1';
             UPDATE messages SET is_starred = 1 WHERE remote_id = 'm2';
             INSERT INTO message_folders (message_id, folder_id)
               SELECT id, {label} FROM messages WHERE remote_id = 'm1';
             DELETE FROM message_folders
               WHERE message_id = (SELECT id FROM messages WHERE remote_id = 'm3');"
        ))
        .unwrap();
    let inbox = Some((FolderKind::Inbox, "INBOX"));
    let targets = store.thread_targets(1, &[t2, t1, 999]).unwrap();
    assert_eq!(
        targets,
        [
            ThreadTarget {
                thread_id: "t1".into(),
                messages: vec![
                    fact("m1", inbox, false, false, 1),
                    fact("m2", inbox, true, true, 2),
                ],
            },
            ThreadTarget {
                thread_id: "t2".into(),
                messages: vec![fact("m3", None, true, false, 3)],
            },
        ],
        "store order; a message's folder is its first; m3 has none"
    );
    assert!(
        store.thread_targets(2, &[t1]).unwrap().is_empty(),
        "another account's threads are not this one's"
    );
}
