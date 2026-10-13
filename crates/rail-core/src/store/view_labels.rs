//! What the label and move pickers read: an account's labels (`l`) or move targets (`m`, by
//! its folder model), and the ones the given threads' messages are in now. Names are outside
//! text.

use std::collections::BTreeSet;
use std::fmt;

use rusqlite::params;

use super::targets::kinds_json;
use super::{Store, StoreError};
use crate::config::FolderModel;
use crate::provider::FolderKind;

/// Labels: user labels first, then Gmail's system ones, each by name.
const LABEL_ORDER: &str = "kind = 'label', name, id";
/// A one-folder mailbox's move targets: Inbox, Archive, Junk, then its own folders by name.
const MOVE_ORDER: &str =
    "CASE kind WHEN 'inbox' THEN 0 WHEN 'archive' THEN 1 WHEN 'spam' THEN 2 ELSE 3 END, name, id";

#[derive(Clone, PartialEq, Eq)]
pub struct LabelOption {
    pub folder_id: i64,
    pub name: String,
}

impl fmt::Debug for LabelOption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LabelOption")
            .field("folder_id", &self.folder_id)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LabelChoices {
    pub labels: Vec<LabelOption>,
    /// The labels any message of the threads carries (the union).
    pub current: BTreeSet<i64>,
}

impl Store {
    /// The account's labels and the union of those `threads` carry. Labels: user labels first,
    /// then Gmail's system ones, each by name, at most `MAX_SIDEBAR_FOLDERS`.
    pub fn label_choices(
        &self,
        account_id: i64,
        threads: &[i64],
    ) -> Result<LabelChoices, StoreError> {
        let kinds = [FolderKind::Label, FolderKind::User];
        self.folder_choices(account_id, threads, &kinds, LABEL_ORDER)
    }

    /// The folders `m` offers for the account's `threads` (`FolderModel::move_kinds`): Gmail's
    /// are its labels, as `label_choices`; a one-folder mailbox lists Inbox, Archive and Junk,
    /// then its own folders by name; at most `MAX_SIDEBAR_FOLDERS`. `current`: the listed
    /// folders the threads' messages are in.
    pub fn move_choices(
        &self,
        account_id: i64,
        threads: &[i64],
    ) -> Result<LabelChoices, StoreError> {
        let model = self.folder_model(account_id)?;
        let order = match model {
            FolderModel::Labels => LABEL_ORDER,
            FolderModel::SingleFolder => MOVE_ORDER,
        };
        self.folder_choices(account_id, threads, model.move_kinds(), order)
    }

    /// The account's folders of `kinds` in `order` (one of this file's constants), and those
    /// `threads` are in.
    fn folder_choices(
        &self,
        account_id: i64,
        threads: &[i64],
        kinds: &[FolderKind],
        order: &str,
    ) -> Result<LabelChoices, StoreError> {
        let kinds = kinds_json(kinds);
        let mut stmt = self.conn.prepare(&format!(
            "SELECT id, name FROM folders WHERE account_id = ?1
               AND kind IN (SELECT value FROM json_each(?2))
             ORDER BY {order} LIMIT ?3"
        ))?;
        let cap = i64::try_from(super::view::MAX_SIDEBAR_FOLDERS).unwrap_or(500);
        let labels = stmt
            .query_map(params![account_id, kinds, cap], |r| {
                Ok(LabelOption {
                    folder_id: r.get(0)?,
                    name: r.get(1)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        let ids = serde_json::to_string(threads).unwrap_or_else(|_| "[]".into());
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT mf.folder_id FROM message_folders mf
             JOIN messages m ON m.id = mf.message_id
             JOIN folders f ON f.id = mf.folder_id
             WHERE m.account_id = ?1 AND f.kind IN (SELECT value FROM json_each(?2))
               AND m.thread_id IN (SELECT value FROM json_each(?3))",
        )?;
        let current = stmt
            .query_map(params![account_id, kinds, ids], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        Ok(LabelChoices { labels, current })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::mock::envelope;
    use crate::provider::{FolderKind, RemoteFolder};
    use crate::sync::batch::Batch;

    #[test]
    fn choices_list_the_accounts_labels_and_the_union_the_threads_carry() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO accounts (name, kind, address) VALUES ('w', 'gmail', 'w@example.com')",
                [],
            )
            .unwrap();
        let f = |id: &str, name: &str, kind| RemoteFolder {
            parent: None,
            remote_id: id.into(),
            name: name.into(),
            kind,
        };
        let mut labelled = envelope("m2", "t2", 2_000);
        labelled.folders.push("Label_2".into());
        let batch = Batch {
            folders: Some(vec![
                f("INBOX", "Inbox", FolderKind::Inbox),
                f("Label_2", "Receipts", FolderKind::User),
                f("Label_1", "Projects", FolderKind::User),
                f("IMPORTANT", "Important", FolderKind::Label),
            ]),
            upserts: vec![envelope("m1", "t1", 1_000), labelled],
            ..Batch::new(1)
        };
        store.apply(&batch, 1).unwrap();
        let threads: Vec<i64> = store
            .conn()
            .prepare("SELECT id FROM threads ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let choices = store.label_choices(1, &threads).unwrap();
        let names: Vec<&str> = choices.labels.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(
            names,
            ["Projects", "Receipts", "Important"],
            "user labels first, no inbox"
        );
        let receipts = choices.labels[1].folder_id;
        assert_eq!(choices.current, BTreeSet::from([receipts]));
        assert!(store.label_choices(2, &threads).unwrap().labels.is_empty());
    }

    #[test]
    fn move_choices_follow_the_accounts_folder_model() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .conn()
            .execute_batch(
                "INSERT INTO accounts (name, kind, address) VALUES ('w', 'gmail', 'w@example.com');
                 INSERT INTO accounts (name, kind, address) VALUES ('c', 'm365', 'c@corp.example');",
            )
            .unwrap();
        let f = |id: &str, name: &str, kind| RemoteFolder {
            parent: None,
            remote_id: id.into(),
            name: name.into(),
            kind,
        };
        let gmail = Batch {
            folders: Some(vec![
                f("INBOX", "Inbox", FolderKind::Inbox),
                f("Label_1", "Projects", FolderKind::User),
                f("IMPORTANT", "Important", FolderKind::Label),
            ]),
            upserts: vec![envelope("m1", "t1", 1_000)],
            ..Batch::new(1)
        };
        store.apply(&gmail, 1).unwrap();
        let mut filed = envelope("m9", "c9", 9_000);
        filed.folders = vec!["f-inbox".into()];
        let corp = Batch {
            folders: Some(vec![
                f("f-zeta", "Zeta", FolderKind::User),
                f("f-sent", "Sent Items", FolderKind::Sent),
                f("f-junk", "Junk Email", FolderKind::Spam),
                f("f-del", "Deleted Items", FolderKind::Trash),
                f("f-alpha", "Alpha", FolderKind::User),
                f("f-arch", "Archive", FolderKind::Archive),
                f("f-drafts", "Drafts", FolderKind::Drafts),
                f("f-inbox", "Inbox", FolderKind::Inbox),
            ]),
            upserts: vec![filed],
            ..Batch::new(2)
        };
        store.apply(&corp, 1).unwrap();
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
        let (t1, c9) = (thread("t1"), thread("c9"));
        assert_eq!(
            store.move_choices(1, &[t1]).unwrap(),
            store.label_choices(1, &[t1]).unwrap(),
            "Gmail moves to its labels, as before"
        );
        let corp = store.move_choices(2, &[c9]).unwrap();
        let names: Vec<&str> = corp.labels.iter().map(|l| l.name.as_str()).collect();
        assert_eq!(names, ["Inbox", "Archive", "Junk Email", "Alpha", "Zeta"]);
        assert_eq!(corp.current, BTreeSet::from([corp.labels[0].folder_id]));
    }
}
