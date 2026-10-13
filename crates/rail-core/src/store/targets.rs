//! What an action is sent for: the threads a row names that the store still holds, each with
//! the facts the scope rule reads of its stored messages (folder kind and id, flags, date). A
//! row's provider call sees those facts as they were before the row's own optimistic change,
//! which the store already shows.

use rusqlite::{Connection, OptionalExtension, params};

use super::apply_local::Prior;
use super::outbox::OutboxRow;
use super::{Store, StoreError};
use crate::provider::{FolderKind, TargetMessage, ThreadTarget};

/// One thread of a target listing: its local id, its target, and the local id of each of the
/// target's messages, in the same order.
pub(crate) struct StoredTarget {
    pub thread: i64,
    pub target: ThreadTarget,
    pub messages: Vec<i64>,
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
                messages: Vec::new(),
            });
        }
        if let (Some((local, facts)), Some(last)) = (facts, targets.last_mut()) {
            last.messages.push(local);
            last.target.messages.push(facts);
        }
    }
    Ok(targets)
}

/// The folders `message` is in now.
pub(crate) fn folders_of(conn: &Connection, message: i64) -> Result<Vec<i64>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT folder_id FROM message_folders WHERE message_id = ?1 ORDER BY folder_id",
    )?;
    let rows = stmt.query_map([message], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The provider id of one of the account's folders.
pub(crate) fn folder_remote_in(
    conn: &Connection,
    account_id: i64,
    folder_id: i64,
) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT remote_id FROM folders WHERE account_id = ?1 AND id = ?2",
            params![account_id, folder_id],
            |r| r.get(0),
        )
        .optional()?)
}

/// A folder's kind and provider id; `None`s for a folder gone since.
fn folder_facts(
    conn: &Connection,
    folder: i64,
) -> Result<(Option<FolderKind>, Option<String>), StoreError> {
    let found: Option<(String, String)> = conn
        .query_row(
            "SELECT kind, remote_id FROM folders WHERE id = ?1",
            [folder],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(match found {
        Some((kind, remote)) => (FolderKind::parse(&kind), Some(remote)),
        None => (None, None),
    })
}

/// `facts` as they were before `prior`'s change: the old flags, and the first folder of the
/// old set (the folders now, less those the change added, plus those it removed).
fn before(conn: &Connection, facts: &mut TargetMessage, prior: &Prior) -> Result<(), StoreError> {
    if let Some(read) = prior.read {
        facts.unread = !read;
    }
    if let Some(starred) = prior.starred {
        facts.starred = starred;
    }
    if prior.added.is_empty() && prior.removed.is_empty() {
        return Ok(());
    }
    let first = folders_of(conn, prior.message)?
        .into_iter()
        .filter(|f| !prior.added.contains(f))
        .chain(prior.removed.iter().copied())
        .min();
    (facts.folder, facts.folder_remote) = match first {
        Some(folder) => folder_facts(conn, folder)?,
        None => (None, None),
    };
    Ok(())
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

    /// The threads of `row` as its provider call must see them: each message's facts as they
    /// were before the row's own optimistic change (its reversal undone), since the store
    /// already shows that change. Threads and messages come in store order.
    pub(crate) fn row_targets(&self, row: &OutboxRow) -> Result<Vec<ThreadTarget>, StoreError> {
        let mut stored = targets_in(&self.conn, row.account_id, &row.thread_ids)?;
        for target in &mut stored {
            let thread = target.thread;
            for (facts, local) in target.target.messages.iter_mut().zip(&target.messages) {
                let prior = row
                    .reversal
                    .iter()
                    .find(|p| p.message == *local && p.thread == thread);
                if let Some(prior) = prior {
                    before(&self.conn, facts, prior)?;
                }
            }
        }
        Ok(stored.into_iter().map(|s| s.target).collect())
    }
}
