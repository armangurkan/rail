//! What an action is sent for: the threads a row names that the store still holds, each with
//! the facts the scope rule reads of its stored messages (folder kind and id, flags, date).

use rusqlite::{Connection, params};

use super::{Store, StoreError};
use crate::provider::{FolderKind, TargetMessage, ThreadTarget};

/// One thread of a target listing: its local id and its target.
pub(crate) struct StoredTarget {
    pub thread: i64,
    pub target: ThreadTarget,
}

/// A message's folder is its lowest-numbered one (Gmail's first label; Graph has one).
const TARGETS: &str = "SELECT t.id, t.remote_id, m.id, m.remote_id, f.kind, f.remote_id,
       m.is_read, m.is_starred, m.date
     FROM threads t
     LEFT JOIN messages m ON m.thread_id = t.id
     LEFT JOIN folders f ON f.id =
       (SELECT min(mf.folder_id) FROM message_folders mf WHERE mf.message_id = m.id)
     WHERE t.account_id = ?1 AND t.id IN (SELECT value FROM json_each(?2))
     ORDER BY t.id, m.id";

/// `(thread, thread remote id, (message, facts))`; no message for a thread with none stored.
type Found = (i64, String, Option<(i64, TargetMessage)>);

fn found(r: &rusqlite::Row<'_>) -> rusqlite::Result<Found> {
    let message: Option<i64> = r.get(2)?;
    let facts = match message {
        Some(local) => {
            let kind: Option<String> = r.get(4)?;
            let read: bool = r.get(6)?;
            let facts = TargetMessage {
                id: r.get(3)?,
                folder: kind.as_deref().and_then(FolderKind::parse),
                folder_remote: r.get(5)?,
                unread: !read,
                starred: r.get(7)?,
                date: r.get(8)?,
            };
            Some((local, facts))
        }
        None => None,
    };
    Ok((r.get(0)?, r.get(1)?, facts))
}

/// The threads of `threads` the store holds for the account, in store order, each with its
/// stored messages in store order.
pub(crate) fn targets_in(
    conn: &Connection,
    account_id: i64,
    threads: &[i64],
) -> Result<Vec<StoredTarget>, StoreError> {
    let ids = serde_json::to_string(threads).unwrap_or_else(|_| "[]".into());
    let mut stmt = conn.prepare(TARGETS)?;
    let rows = stmt.query_map(params![account_id, ids], found)?;
    let mut targets: Vec<StoredTarget> = Vec::new();
    for row in rows {
        let (thread, remote, facts) = row?;
        if targets.last().is_none_or(|t| t.thread != thread) {
            targets.push(StoredTarget {
                thread,
                target: ThreadTarget::from(remote),
            });
        }
        if let (Some((_, facts)), Some(last)) = (facts, targets.last_mut()) {
            last.target.messages.push(facts);
        }
    }
    Ok(targets)
}

impl Store {
    /// The threads of `threads` the store still holds for the account, each with the facts of
    /// its stored messages as the store holds them now. Threads and messages come in store
    /// order.
    pub fn thread_targets(
        &self,
        account_id: i64,
        threads: &[i64],
    ) -> Result<Vec<ThreadTarget>, StoreError> {
        Ok(targets_in(&self.conn, account_id, threads)?
            .into_iter()
            .map(|s| s.target)
            .collect())
    }
}
