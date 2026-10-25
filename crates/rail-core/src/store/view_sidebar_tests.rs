//! The sidebar's folder cap applies to each account on its own, so one account with many
//! labels cannot hide another account's folders.

use super::Store;
use super::view::MAX_SIDEBAR_FOLDERS;
use crate::provider::{FolderKind, RemoteFolder};
use crate::sync::batch::Batch;

const NOW: i64 = 1_760_100_000;

/// The inbox and `count` user labels named `{prefix} 0000`, `{prefix} 0001`, ...
fn labels(prefix: &str, count: usize) -> Vec<RemoteFolder> {
    let inbox = RemoteFolder {
        parent: None,
        remote_id: "INBOX".into(),
        name: "Inbox".into(),
        kind: FolderKind::Inbox,
    };
    let user = (0..count).map(|n| RemoteFolder {
        parent: None,
        remote_id: format!("Label_{n}"),
        name: format!("{prefix} {n:04}"),
        kind: FolderKind::User,
    });
    std::iter::once(inbox).chain(user).collect()
}

#[test]
fn the_folder_cap_applies_to_each_account_on_its_own() {
    let mut store = Store::open_in_memory().unwrap();
    store
        .conn()
        .execute_batch(
            "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', 'w@example.com');
             INSERT INTO accounts (name, kind, address) VALUES ('home', 'gmail', 'h@example.com');",
        )
        .unwrap();
    for (account, folders) in [
        (1, labels("Work", MAX_SIDEBAR_FOLDERS + 20)),
        (2, labels("Home", 3)),
    ] {
        let batch = Batch {
            folders: Some(folders),
            ..Batch::new(account)
        };
        store.apply(&batch, NOW).unwrap();
    }
    let counts = store.sidebar_counts(&[1, 2], &[], 0).unwrap();
    let of = |account: i64| {
        counts
            .folders
            .iter()
            .filter(|f| f.account_id == account)
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        of(1).len(),
        MAX_SIDEBAR_FOLDERS,
        "the busy account is capped"
    );
    assert_eq!(of(1)[..2], ["Inbox", "Work 0000"], "kind first, then name");
    assert_eq!(
        of(2),
        ["Inbox", "Home 0000", "Home 0001", "Home 0002"],
        "the other account keeps every folder"
    );
    let mut accounts: Vec<i64> = counts.folders.iter().map(|f| f.account_id).collect();
    accounts.dedup();
    assert_eq!(accounts, [1, 2], "grouped by account, in order");
}
