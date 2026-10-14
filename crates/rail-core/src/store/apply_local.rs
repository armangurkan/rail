//! The optimistic local change of an action, and its exact reverse. `apply` follows the
//! account's folder model (Gmail's labels here, one folder per message in `apply_single`) and
//! records, per message, only what it changed (the old flag, the folders it added or removed,
//! each folder's provider id); `revert` puts that back, touching a message only while it still
//! belongs to the thread recorded (rowids can be reused) and a folder only while it still has
//! the provider id recorded. `restack` drops or brings back rows among overlapping ones;
//! `reapply_live` is the conflict rule; `forget_threads` removes deleted threads from rows.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};

use super::accounts::folder_model_in;
use super::outbox::{live_rows_touching, save_threads_and_reversal};
use super::{StoreError, rollup};
use crate::config::FolderModel;
use crate::ops::{Action, OutboxId};

/// What an action changed on one message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prior {
    pub message: i64,
    pub thread: i64,
    /// The old `is_read`, when the action changed it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starred: Option<bool>,
    /// Folders the action added (a revert removes them).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub added: Vec<i64>,
    /// Folders the action removed (a revert adds them back).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed: Vec<i64>,
    /// The provider id each folder of `added` and `removed` had when the action ran: a revert
    /// finds the folder by it, since SQLite can give a deleted folder's id to a new one.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub remotes: BTreeMap<i64, String>,
}

impl Prior {
    /// True when the action changed anything on the message.
    pub(super) fn changed(&self) -> bool {
        self.read.is_some()
            || self.starred.is_some()
            || !self.added.is_empty()
            || !self.removed.is_empty()
    }
}

/// One action as folder and flag changes.
#[derive(Default)]
struct Change {
    add: Vec<i64>,
    remove: Vec<i64>,
    read: Option<bool>,
    starred: Option<bool>,
}

/// The account's folder of `kind` (the lowest-numbered one).
pub(super) fn system_folder(
    conn: &Connection,
    account: i64,
    kind: &str,
) -> Result<Option<i64>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT id FROM folders WHERE account_id = ?1 AND kind = ?2 ORDER BY id LIMIT 1",
            params![account, kind],
            |r| r.get(0),
        )
        .optional()?)
}

fn change_for(tx: &Transaction, account: i64, action: &Action) -> Result<Change, StoreError> {
    let inbox: Vec<i64> = system_folder(tx, account, "inbox")?.into_iter().collect();
    Ok(match action {
        Action::Archive {} => Change {
            remove: inbox,
            ..Change::default()
        },
        Action::Trash {} => Change {
            add: system_folder(tx, account, "trash")?.into_iter().collect(),
            remove: inbox,
            ..Change::default()
        },
        Action::Star { on } => Change {
            starred: Some(*on),
            ..Change::default()
        },
        Action::Read { on } => Change {
            read: Some(*on),
            ..Change::default()
        },
        Action::Labels { add, remove } => Change {
            add: add.clone(),
            remove: remove.clone(),
            ..Change::default()
        },
        Action::Move { to } => Change {
            add: vec![*to],
            remove: inbox,
            ..Change::default()
        },
        // Draft actions name no threads; the draft's state is the outbox's to set.
        Action::Send { .. } | Action::PushDraft { .. } | Action::DeleteDraft { .. } => {
            Change::default()
        }
    })
}

/// `(message, thread, is_read, is_starred)` of every message of `threads` that are `account`'s.
fn messages_of(
    tx: &Transaction,
    account: i64,
    threads: &[i64],
) -> Result<Vec<(i64, i64, bool, bool)>, StoreError> {
    let ids = serde_json::to_string(threads).unwrap_or_else(|_| "[]".into());
    let mut stmt = tx.prepare(
        "SELECT m.id, m.thread_id, m.is_read, m.is_starred FROM messages m
         JOIN threads t ON t.id = m.thread_id
         WHERE t.account_id = ?1 AND m.thread_id IN (SELECT value FROM json_each(?2)) ORDER BY m.id",
    )?;
    let rows = stmt.query_map(params![account, ids], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub(super) fn in_folder(tx: &Transaction, message: i64, folder: i64) -> Result<bool, StoreError> {
    Ok(tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM message_folders WHERE message_id = ?1 AND folder_id = ?2)",
        params![message, folder],
        |r| r.get(0),
    )?)
}

/// Add `folder` to `message` when both still exist and the message is in `thread` (a folder
/// deleted by a labels sync, or a reused rowid, adds nothing).
pub(super) fn add_membership(
    tx: &Transaction,
    message: i64,
    thread: i64,
    folder: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT OR IGNORE INTO message_folders (message_id, folder_id)
         SELECT m.id, f.id FROM messages m, folders f
         WHERE m.id = ?1 AND m.thread_id = ?2 AND f.id = ?3",
        params![message, thread, folder],
    )?;
    Ok(())
}

pub(super) fn remove_membership(
    tx: &Transaction,
    message: i64,
    thread: i64,
    folder: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM message_folders WHERE message_id = ?1 AND folder_id = ?3
         AND EXISTS (SELECT 1 FROM messages WHERE id = ?1 AND thread_id = ?2)",
        params![message, thread, folder],
    )?;
    Ok(())
}

pub(super) fn set_flag(
    tx: &Transaction,
    column: &str,
    message: i64,
    thread: i64,
    on: bool,
) -> Result<(), StoreError> {
    // `column` is Rail's own: `is_read` or `is_starred`.
    tx.execute(
        &format!("UPDATE messages SET {column} = ?3 WHERE id = ?1 AND thread_id = ?2"),
        params![message, thread, on],
    )?;
    Ok(())
}

/// Apply `action` to the messages of `threads` its account's folder model picks; what it
/// changed, with each folder's provider id. Rollups are the caller's.
pub(crate) fn apply(
    tx: &Transaction,
    account: i64,
    action: &Action,
    threads: &[i64],
) -> Result<Vec<Prior>, StoreError> {
    let priors = match folder_model_in(tx, account)? {
        FolderModel::Labels => apply_labels(tx, account, action, threads)?,
        FolderModel::SingleFolder => super::apply_single::apply(tx, account, action, threads)?,
    };
    with_remote_ids(tx, priors)
}

/// `priors` with the provider id of every folder they name, read now.
fn with_remote_ids(tx: &Transaction, mut priors: Vec<Prior>) -> Result<Vec<Prior>, StoreError> {
    for p in &mut priors {
        for &folder in p.added.iter().chain(&p.removed) {
            if let Some(remote) = folder_remote(tx, folder)? {
                p.remotes.entry(folder).or_insert(remote);
            }
        }
    }
    Ok(priors)
}

fn folder_remote(tx: &Transaction, folder: i64) -> Result<Option<String>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT remote_id FROM folders WHERE id = ?1",
            [folder],
            |r| r.get(0),
        )
        .optional()?)
}

/// The folder `p` recorded as `folder`, as the store numbers it now: itself while it is still
/// the account's and has the provider id recorded, else the account's folder holding that id
/// now; `None`, logged, when none does. Both lookups stay in `account`: Gmail's label ids repeat
/// across accounts. A record from before ids were kept names the folder as it is.
fn resolved(
    tx: &Transaction,
    account: i64,
    p: &Prior,
    folder: i64,
) -> Result<Option<i64>, StoreError> {
    let Some(remote) = p.remotes.get(&folder) else {
        return Ok(Some(folder));
    };
    if super::targets::folder_remote_in(tx, account, folder)?.as_ref() == Some(remote) {
        return Ok(Some(folder));
    }
    let now: Option<i64> = tx
        .query_row(
            "SELECT id FROM folders WHERE account_id = ?1 AND remote_id = ?2",
            params![account, remote],
            |r| r.get(0),
        )
        .optional()?;
    if now.is_none() {
        tracing::info!(
            message = p.message,
            folder,
            "a folder an undo would restore is gone; the message is not put back in it"
        );
    }
    Ok(now)
}

/// Gmail's labels: `action` on every message of `threads`.
fn apply_labels(
    tx: &Transaction,
    account: i64,
    action: &Action,
    threads: &[i64],
) -> Result<Vec<Prior>, StoreError> {
    let change = change_for(tx, account, action)?;
    let mut priors = Vec::new();
    for (message, thread, read, starred) in messages_of(tx, account, threads)? {
        let mut prior = Prior {
            message,
            thread,
            ..Prior::default()
        };
        if let Some(on) = change.read.filter(|on| *on != read) {
            set_flag(tx, "is_read", message, thread, on)?;
            prior.read = Some(read);
        }
        if let Some(on) = change.starred.filter(|on| *on != starred) {
            set_flag(tx, "is_starred", message, thread, on)?;
            prior.starred = Some(starred);
        }
        for &folder in &change.add {
            if !in_folder(tx, message, folder)? {
                add_membership(tx, message, thread, folder)?;
                prior.added.push(folder);
            }
        }
        for &folder in &change.remove {
            if in_folder(tx, message, folder)? {
                remove_membership(tx, message, thread, folder)?;
                prior.removed.push(folder);
            }
        }
        if prior.changed() {
            priors.push(prior);
        }
    }
    Ok(priors)
}

/// True when `p` moved the message into a folder and the server has since put it in another:
/// on a one-folder mailbox the server's folder then stands and `p`'s folder part is stale. A
/// message in no folder at all (the folder `p` put it in was deleted, and its membership with
/// it) has not left: its old folder goes back.
fn left_its_folder(tx: &Transaction, p: &Prior) -> Result<bool, StoreError> {
    if p.added.is_empty() {
        return Ok(false);
    }
    for &folder in &p.added {
        if in_folder(tx, p.message, folder)? {
            return Ok(false);
        }
    }
    Ok(!super::targets::folders_of(tx, p.message)?.is_empty())
}

/// Put back what `priors` record of `account`, each folder found by its provider id. On a
/// one-folder mailbox the folder part goes back only while the message is still in the folder
/// the action put it in, and only one folder goes back: the oldest prior's (`merge` keeps it
/// first).
pub(crate) fn revert(tx: &Transaction, account: i64, priors: &[Prior]) -> Result<(), StoreError> {
    let single = folder_model_in(tx, account)? == FolderModel::SingleFolder;
    for p in priors {
        if let Some(read) = p.read {
            set_flag(tx, "is_read", p.message, p.thread, read)?;
        }
        if let Some(starred) = p.starred {
            set_flag(tx, "is_starred", p.message, p.thread, starred)?;
        }
        if single && left_its_folder(tx, p)? {
            continue;
        }
        for &folder in &p.added {
            if let Some(folder) = resolved(tx, account, p, folder)? {
                remove_membership(tx, p.message, p.thread, folder)?;
            }
        }
        let back = if single {
            &p.removed[..p.removed.len().min(1)]
        } else {
            &p.removed[..]
        };
        for &folder in back {
            if let Some(folder) = resolved(tx, account, p, folder)? {
                add_membership(tx, p.message, p.thread, folder)?;
            }
        }
    }
    Ok(())
}

/// `new` folded into `old`, per message and field, the older value winning: it is the state
/// before the action (plan decision 4).
fn merge(mut old: Vec<Prior>, new: Vec<Prior>) -> Vec<Prior> {
    for p in new {
        match old.iter_mut().find(|o| o.message == p.message) {
            Some(o) => {
                o.read = o.read.or(p.read);
                o.starred = o.starred.or(p.starred);
                for f in p.added {
                    if !o.added.contains(&f) {
                        o.added.push(f);
                    }
                }
                for f in p.removed {
                    if !o.removed.contains(&f) {
                        o.removed.push(f);
                    }
                }
                for (folder, remote) in p.remotes {
                    o.remotes.entry(folder).or_insert(remote);
                }
            }
            None => old.push(p),
        }
    }
    old
}

fn within(threads: &[i64], set: &BTreeSet<i64>) -> Vec<i64> {
    threads
        .iter()
        .copied()
        .filter(|t| set.contains(t))
        .collect()
}

/// Revert every live row touching `threads` newest first, leave the `drop` rows out, apply
/// the rest again oldest first with fresh reversals, all restricted to `threads`; then the
/// rollups. Used by undo (drop the undone rows), permanent failure (drop the failed row) and
/// retry (drop nothing: the retried row, now pending with an empty reversal, joins in order).
pub(crate) fn restack(
    tx: &Transaction,
    threads: &BTreeSet<i64>,
    drop: &[OutboxId],
) -> Result<(), StoreError> {
    let rows = live_rows_touching(tx, threads, drop)?;
    for row in rows.iter().rev() {
        let mine: Vec<Prior> = row
            .reversal
            .iter()
            .filter(|p| threads.contains(&p.thread))
            .cloned()
            .collect();
        revert(tx, row.account_id, &mine)?;
    }
    for row in &rows {
        let mut kept: Vec<Prior> = row
            .reversal
            .iter()
            .filter(|p| !threads.contains(&p.thread))
            .cloned()
            .collect();
        if !drop.contains(&row.id) {
            kept.extend(apply(
                tx,
                row.account_id,
                &row.action,
                &within(&row.thread_ids, threads),
            )?);
        }
        save_threads_and_reversal(tx, row.id, &row.thread_ids, &kept)?;
    }
    rollup::refresh(tx, threads)
}

/// On a one-folder mailbox, `old` without the folder part of a prior whose message the fresh
/// reapply left untouched and the server has since moved out of the folder it was put in (the
/// server's folder stands); a prior left with nothing to put back goes. Gmail's labels: as is.
fn drop_stale_folder_parts(
    tx: &Transaction,
    account: i64,
    old: Vec<Prior>,
    fresh: &[Prior],
) -> Result<Vec<Prior>, StoreError> {
    if folder_model_in(tx, account)? != FolderModel::SingleFolder {
        return Ok(old);
    }
    let mut kept = Vec::with_capacity(old.len());
    for mut p in old {
        let untouched = !fresh.iter().any(|f| f.message == p.message);
        if untouched && left_its_folder(tx, &p)? {
            p.added.clear();
            p.removed.clear();
        }
        if p.changed() {
            kept.push(p);
        }
    }
    Ok(kept)
}

/// The conflict rule: after a sync batch wrote server state for `touched`, apply every live
/// row of `account` on those threads again, oldest first, merging the reversals. Rollups are
/// the caller's (the batch's own rollup runs next).
pub(crate) fn reapply_live(
    tx: &Transaction,
    account: i64,
    touched: &BTreeSet<i64>,
) -> Result<(), StoreError> {
    for row in live_rows_touching(tx, touched, &[])? {
        if row.account_id != account {
            continue;
        }
        let fresh = apply(tx, account, &row.action, &within(&row.thread_ids, touched))?;
        let old = drop_stale_folder_parts(tx, account, row.reversal, &fresh)?;
        save_threads_and_reversal(tx, row.id, &row.thread_ids, &merge(old, fresh))?;
    }
    Ok(())
}

/// After a rollup: threads of `touched` that no longer exist leave every outbox row (live or
/// failed; a row left with no thread is deleted), with their reversal entries, so a reused
/// rowid is never acted on.
pub(crate) fn forget_threads(tx: &Transaction, touched: &BTreeSet<i64>) -> Result<(), StoreError> {
    let mut gone = BTreeSet::new();
    for &thread in touched {
        let exists: bool = tx.query_row(
            "SELECT EXISTS (SELECT 1 FROM threads WHERE id = ?1)",
            [thread],
            |r| r.get(0),
        )?;
        if !exists {
            gone.insert(thread);
        }
    }
    if gone.is_empty() {
        return Ok(());
    }
    let mut by_row: BTreeMap<OutboxId, (Vec<i64>, Vec<Prior>)> = BTreeMap::new();
    for row in super::outbox::rows_touching(tx, &gone)? {
        let threads = row
            .thread_ids
            .iter()
            .copied()
            .filter(|t| !gone.contains(t))
            .collect();
        let reversal = row
            .reversal
            .into_iter()
            .filter(|p| !gone.contains(&p.thread))
            .collect();
        by_row.insert(row.id, (threads, reversal));
    }
    for (id, (threads, reversal)) in by_row {
        if threads.is_empty() {
            // Nothing left to send or revert.
            tx.execute("DELETE FROM outbox WHERE id = ?1", [id])?;
        } else {
            save_threads_and_reversal(tx, id, &threads, &reversal)?;
        }
    }
    Ok(())
}
