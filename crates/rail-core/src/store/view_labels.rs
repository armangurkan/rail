//! What the label pickers read: an account's labels (kind `label` or `user`, by name) and the
//! labels the given threads' messages carry now. Names are outside text.

use std::collections::BTreeSet;
use std::fmt;

use rusqlite::params;

use super::{Store, StoreError};

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
        let mut stmt = self.conn.prepare(
            "SELECT id, name FROM folders WHERE account_id = ?1 AND kind IN ('label', 'user')
             ORDER BY kind = 'label', name, id LIMIT ?2",
        )?;
        let cap = i64::try_from(super::view::MAX_SIDEBAR_FOLDERS).unwrap_or(500);
        let labels = stmt
            .query_map(params![account_id, cap], |r| {
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
             WHERE m.account_id = ?1 AND f.kind IN ('label', 'user')
               AND m.thread_id IN (SELECT value FROM json_each(?2))",
        )?;
        let current = stmt
            .query_map(params![account_id, ids], |r| r.get(0))?
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
}
