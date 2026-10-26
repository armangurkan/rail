//! Snoozes: a thread taken out of the inbox now and put back at a time. A snooze is keyed by the
//! provider's thread id (rowids are reused) and linked to its archive's outbox row while that row
//! lives, so an undo, a sync settling the archive's failure or a forgotten thread deletes it
//! with the row. Completing the archive records the messages it took out of the inbox and clears
//! the link: the snooze is then active until its time, an incoming message or a clear. Its state
//! is read from the link and `woken_at`, never stored. A wake queues two ordinary outbox rows (the
//! return of the recorded messages still archived, then mark unread) and dates the thread at the
//! wake, so it sorts first until it leaves the inbox: Rail archives, trashes, moves or snoozes it
//! again, or a sync finds it gone from the inbox. A thread the store forgets takes its snooze.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};

use super::accounts::folder_model_in;
use super::apply_local::{Prior, system_folder};
use super::outbox::{NewAction, OutboxOutcome, enqueue, json};
use super::{Store, StoreError, rollup};
use crate::config::FolderModel;
use crate::ops::{Action, OutboxId};

/// Messages of a thread that `prune_old` and `sweep_unseen` keep: a snoozed thread stays whole
/// until it comes back.
pub(super) const NOT_SNOOZED: &str = "NOT EXISTS (SELECT 1 FROM threads st JOIN snoozes s
    ON s.account_id = st.account_id AND s.thread_remote = st.remote_id
    WHERE st.id = messages.thread_id)";

/// A snooze to write: its threads, its time, and its archive's due time and client token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnoozeNew {
    /// The account the threads belong to.
    pub account_id: i64,
    /// The store's thread ids; a thread named twice is snoozed once.
    pub threads: Vec<i64>,
    /// When the threads come back, in Unix seconds.
    pub until: i64,
    /// When the archive is due (the undo grace).
    pub not_before: i64,
    /// The client's token: a replay finds the archive it made.
    pub client_token: String,
}

/// Where a snooze is, read from its archive row and `woken_at`. `Store::snoozes` and
/// `Store::snooze_of_thread` return every state; a thread counts as snoozed while its snooze is
/// `Pending` or `Active`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnoozeState {
    /// The archive waits or is being sent.
    Pending,
    /// The archive was refused or gave up; a retry brings the snooze back.
    Failed,
    /// Archived, waiting for its time.
    Active,
    /// Back in the inbox since `woken_at`.
    Woken,
}

/// One snooze, as the store reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnoozeRow {
    pub id: i64,
    pub account_id: i64,
    /// The thread's provider id.
    pub thread_remote: String,
    /// When it comes back, in Unix seconds.
    pub until: i64,
    pub state: SnoozeState,
    /// When it went back, once it has.
    pub woken_at: Option<i64>,
    /// The provider ids the archive took out of the inbox, sorted.
    pub messages: Vec<String>,
}

const ROW: &str = "SELECT s.id, s.account_id, s.thread_remote, s.until, s.woken_at,
    CASE WHEN s.woken_at IS NOT NULL THEN 'woken' WHEN s.archive_row IS NULL THEN 'active'
         WHEN o.state = 'failed' THEN 'failed' ELSE 'pending' END,
    (SELECT json_group_array(message_remote) FROM
       (SELECT message_remote FROM snooze_messages WHERE snooze_id = s.id ORDER BY message_remote)),
    s.archive_row
  FROM snoozes s LEFT JOIN outbox o ON o.id = s.archive_row";

fn read_row(r: &Row<'_>) -> rusqlite::Result<SnoozeRow> {
    let state: String = r.get(5)?;
    let messages: String = r.get(6)?;
    Ok(SnoozeRow {
        id: r.get(0)?,
        account_id: r.get(1)?,
        thread_remote: r.get(2)?,
        until: r.get(3)?,
        woken_at: r.get(4)?,
        state: match state.as_str() {
            "woken" => SnoozeState::Woken,
            "active" => SnoozeState::Active,
            "failed" => SnoozeState::Failed,
            _ => SnoozeState::Pending,
        },
        // Rail's own JSON; an unreadable list (a hand-edited store) returns nothing.
        messages: serde_json::from_str(&messages).unwrap_or_default(),
    })
}

/// `OutboxWrite::Snooze`: a thread already snoozed takes the new time (`again`); the rest that
/// hold mail in the inbox are archived by one row (the client's token) and snoozed. A token the
/// outbox already holds answers with its row and how many of the threads are snoozed now, and
/// changes nothing.
pub(super) fn snooze(
    tx: &Transaction,
    new: &SnoozeNew,
    now: i64,
) -> Result<OutboxOutcome, StoreError> {
    let threads = unique(&new.threads);
    if let Some(row) = known_row(tx, &new.client_token)? {
        let snoozed = holding(tx, new.account_id, &threads)?;
        return Ok(OutboxOutcome::Snoozed {
            rows: vec![row],
            snoozed,
        });
    }
    let (mut snoozed, mut fresh) = (0, Vec::new());
    for thread in threads {
        let Some(remote) = thread_remote(tx, new.account_id, thread)? else {
            continue;
        };
        if again(tx, new, (thread, &remote), now)? {
            snoozed += 1;
        } else if in_inbox(tx, thread)? {
            fresh.push((thread, remote));
        }
    }
    let rows = archive_and_record(tx, new, &fresh, now)?;
    if !rows.is_empty() {
        snoozed += fresh.len();
    }
    Ok(OutboxOutcome::Snoozed { rows, snoozed })
}

/// A thread snoozed before: true when it only takes the new time (its snooze is pending, or
/// active with nothing back in the inbox). Otherwise the old snooze makes way for a fresh one:
/// a failed one's archive is dropped, an active one with mail back in the inbox puts the rest of
/// its mail back first (as a wake would, so the fresh archive takes it all), a woken one goes.
fn again(
    tx: &Transaction,
    new: &SnoozeNew,
    (thread, remote): (i64, &str),
    now: i64,
) -> Result<bool, StoreError> {
    let Some((old, archive)) = current(tx, new.account_id, remote)? else {
        return Ok(false);
    };
    match old.state {
        SnoozeState::Pending => set_until(tx, old.id, new.until)?,
        SnoozeState::Active if !in_inbox(tx, thread)? => set_until(tx, old.id, new.until)?,
        SnoozeState::Active => {
            return_thread(tx, new.account_id, old.id, remote, now)?;
            forget_woken(tx, new.account_id, remote)?;
            return Ok(false);
        }
        SnoozeState::Failed => {
            drop_failed(tx, archive)?;
            return Ok(false);
        }
        SnoozeState::Woken => {
            forget_woken(tx, new.account_id, remote)?;
            return Ok(false);
        }
    }
    Ok(true)
}

/// The account's snooze of the thread `remote`, with its archive row while that lives.
fn current(
    tx: &Transaction,
    account: i64,
    remote: &str,
) -> Result<Option<(SnoozeRow, Option<OutboxId>)>, StoreError> {
    Ok(tx
        .query_row(
            &format!("{ROW} WHERE s.account_id = ?1 AND s.thread_remote = ?2"),
            params![account, remote],
            |r| Ok((read_row(r)?, r.get(7)?)),
        )
        .optional()?)
}

fn set_until(tx: &Transaction, snooze: i64, until: i64) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE snoozes SET until = ?2 WHERE id = ?1",
        params![snooze, until],
    )?;
    Ok(())
}

/// A failed snooze's archive row goes, and its snooze with it (the row's cascade): neither a
/// retry nor the next sync can send that archive any more. Its local change was reverted when
/// it failed.
fn drop_failed(tx: &Transaction, archive: Option<OutboxId>) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM outbox WHERE id = ?1 AND state = 'failed'",
        [archive],
    )?;
    Ok(())
}

/// How many of `threads` (the account's) a snooze holds now: pending or active.
fn holding(tx: &Transaction, account: i64, threads: &[i64]) -> Result<usize, StoreError> {
    let mut count = 0;
    for &thread in threads {
        let Some(remote) = thread_remote(tx, account, thread)? else {
            continue;
        };
        let state = current(tx, account, &remote)?.map(|(row, _)| row.state);
        count += usize::from(matches!(
            state,
            Some(SnoozeState::Pending | SnoozeState::Active)
        ));
    }
    Ok(count)
}

/// Each thread once, in order.
fn unique(threads: &[i64]) -> Vec<i64> {
    let mut seen = BTreeSet::new();
    threads
        .iter()
        .copied()
        .filter(|t| seen.insert(*t))
        .collect()
}

/// One archive row for `fresh` and a pending snooze per thread, linked to it.
fn archive_and_record(
    tx: &Transaction,
    new: &SnoozeNew,
    fresh: &[(i64, String)],
    now: i64,
) -> Result<Vec<OutboxId>, StoreError> {
    if fresh.is_empty() {
        return Ok(Vec::new());
    }
    let archive = NewAction {
        account_id: new.account_id,
        action: Action::Archive {},
        threads: fresh.iter().map(|(t, _)| *t).collect(),
        not_before: new.not_before,
        client_token: new.client_token.clone(),
    };
    let Some((row, _)) = enqueue(tx, &archive)? else {
        return Ok(Vec::new());
    };
    for (_, remote) in fresh {
        tx.execute(
            "INSERT INTO snoozes (account_id, thread_remote, until, archive_row, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![new.account_id, remote, new.until, row, now],
        )?;
    }
    Ok(vec![row])
}

/// The archive `row` was sent: its snoozes become active, recording the messages it took out of
/// the inbox. Its final reversal also holds mail that arrived while it waited (`reapply_live`).
pub(super) fn archive_sent(tx: &Transaction, row: OutboxId) -> Result<(), StoreError> {
    let snoozes = linked(tx, row)?;
    if snoozes.is_empty() {
        return Ok(());
    }
    let reversal: String =
        tx.query_row("SELECT reversal FROM outbox WHERE id = ?1", [row], |r| {
            r.get(0)
        })?;
    // Rail's own JSON. An unreadable one (a hand-edited store) records nothing, so it is logged:
    // that snooze has no mail to put back.
    let priors: Vec<Prior> = serde_json::from_str(&reversal).unwrap_or_else(|_| {
        tracing::warn!(row, "a snoozed archive's reversal did not parse");
        Vec::new()
    });
    for (snooze, account, remote) in snoozes {
        let (Some(thread), Some(inbox)) = (
            thread_id(tx, account, &remote)?,
            system_folder(tx, account, "inbox")?,
        ) else {
            continue;
        };
        for p in priors
            .iter()
            .filter(|p| p.thread == thread && p.removed.contains(&inbox))
        {
            tx.execute(
                "INSERT OR IGNORE INTO snooze_messages (snooze_id, message_remote)
                 SELECT ?1, remote_id FROM messages WHERE id = ?2 AND thread_id = ?3",
                params![snooze, p.message, thread],
            )?;
        }
    }
    tx.execute(
        "UPDATE snoozes SET archive_row = NULL WHERE archive_row = ?1",
        [row],
    )?;
    Ok(())
}

/// `OutboxWrite::Wake`: every active snooze of `account` due by `now` goes back; the threads
/// that did.
pub(super) fn wake(tx: &Transaction, account: i64, now: i64) -> Result<OutboxOutcome, StoreError> {
    let mut stmt = tx.prepare(
        "SELECT id, thread_remote FROM snoozes WHERE account_id = ?1 AND archive_row IS NULL
           AND woken_at IS NULL AND until <= ?2 ORDER BY until, id",
    )?;
    let due: Vec<(i64, String)> = stmt
        .query_map(params![account, now], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    drop(stmt);
    let mut threads = Vec::new();
    for (snooze, remote) in due {
        threads.extend(return_thread(tx, account, snooze, &remote, now)?);
    }
    Ok(OutboxOutcome::Woke { threads })
}

/// `OutboxWrite::Unsnooze`, "Return to inbox now": an active snooze goes back at once; a pending
/// one is due now (it goes back once its archive is sent); a failed one is dropped with its
/// archive (reverted already), so no retry archives the thread later. A thread with no snooze,
/// or one back already, is left alone. How many threads were returned or made due.
pub(super) fn clear(
    tx: &Transaction,
    account: i64,
    threads: &[i64],
    now: i64,
) -> Result<OutboxOutcome, StoreError> {
    let mut returned = 0;
    for thread in unique(threads) {
        let Some(remote) = thread_remote(tx, account, thread)? else {
            continue;
        };
        let Some((snooze, archive)) = current(tx, account, &remote)? else {
            continue;
        };
        returned += match snooze.state {
            SnoozeState::Active => {
                usize::from(return_thread(tx, account, snooze.id, &remote, now)?.is_some())
            }
            SnoozeState::Pending => {
                set_until(tx, snooze.id, now)?;
                1
            }
            SnoozeState::Failed => {
                drop_failed(tx, archive)?;
                0
            }
            SnoozeState::Woken => 0,
        };
    }
    Ok(OutboxOutcome::Unsnoozed { returned })
}

/// Put one snooze's thread back now: queue the return of its recorded messages still archived,
/// then mark it unread, and date it at `now`. A snooze with nothing left to return (the mail
/// was deleted, reported or filed since) goes without a row. The thread when it went back.
fn return_thread(
    tx: &Transaction,
    account: i64,
    snooze: i64,
    remote: &str,
    now: i64,
) -> Result<Option<i64>, StoreError> {
    let thread = thread_id(tx, account, remote)?;
    let archived = match thread {
        Some(thread) => still_archived(tx, account, snooze, thread)?,
        None => Vec::new(),
    };
    let Some(thread) = thread.filter(|_| !archived.is_empty()) else {
        tx.execute("DELETE FROM snoozes WHERE id = ?1", [snooze])?;
        return Ok(None);
    };
    let rows = [
        (Action::Unsnooze { messages: archived }, "unsnooze"),
        (Action::Read { on: false }, "unsnooze-read"),
    ];
    for (action, prefix) in rows {
        let row = NewAction {
            account_id: account,
            action,
            threads: vec![thread],
            not_before: now,
            client_token: format!("{prefix}-{snooze}-{now}"),
        };
        enqueue(tx, &row)?;
    }
    tx.execute(
        "UPDATE snoozes SET woken_at = ?2 WHERE id = ?1",
        params![snooze, now],
    )?;
    rollup::refresh(tx, &BTreeSet::from([thread]))?;
    Ok(Some(thread))
}

/// The snooze's recorded messages still archived in `thread`: out of the inbox and not in Trash
/// or Spam (Gmail's labels), or in Archive (a one-folder mailbox, as the scope rule reads it).
fn still_archived(
    tx: &Transaction,
    account: i64,
    snooze: i64,
    thread: i64,
) -> Result<Vec<String>, StoreError> {
    let place = match folder_model_in(tx, account)? {
        FolderModel::Labels => "NOT EXISTS (SELECT 1 FROM message_folders mf JOIN folders f
             ON f.id = mf.folder_id WHERE mf.message_id = m.id AND f.kind IN ('inbox', 'trash', 'spam'))",
        FolderModel::SingleFolder => "EXISTS (SELECT 1 FROM message_folders mf JOIN folders f
             ON f.id = mf.folder_id WHERE mf.message_id = m.id AND f.kind = 'archive')",
    };
    let mut stmt = tx.prepare(&format!(
        "SELECT sm.message_remote FROM snooze_messages sm
         JOIN messages m ON m.remote_id = sm.message_remote AND m.thread_id = ?2
         WHERE sm.snooze_id = ?1 AND {place} ORDER BY sm.message_remote"
    ))?;
    let rows = stmt.query_map(params![snooze, thread], |r| r.get(0))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// `incoming`'s statement (parameters: the account, the batch's provider ids as JSON, now).
/// It starts from the account's active snoozes (few) and reaches the batch's messages by their
/// provider ids, so its cost does not grow with the mailbox.
pub(super) const INCOMING: &str = "UPDATE snoozes SET until = ?3
    WHERE account_id = ?1 AND archive_row IS NULL AND woken_at IS NULL AND until > ?3
      AND EXISTS (
        SELECT 1 FROM messages m JOIN threads t ON t.id = m.thread_id
        WHERE m.account_id = ?1 AND m.remote_id IN (SELECT value FROM json_each(?2))
          AND t.remote_id = snoozes.thread_remote
          AND EXISTS (SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
                      WHERE mf.message_id = m.id AND f.kind = 'inbox')
          AND NOT EXISTS (SELECT 1 FROM message_folders mf JOIN folders f
                          ON f.id = mf.folder_id
                          WHERE mf.message_id = m.id AND f.kind IN ('sent', 'drafts'))
          AND NOT EXISTS (SELECT 1 FROM snooze_messages sm WHERE sm.snooze_id = snoozes.id
                          AND sm.message_remote = m.remote_id))";

/// A sync batch put `messages` (provider ids) somewhere: any of them now in the inbox, not in
/// Sent or Drafts and not recorded by the thread's active snooze makes that snooze due now. Rail
/// stores no sender, so the folder tells: the user's own reply lands in Sent and never counts.
/// Pending snoozes are left alone: their archive takes the new mail with it. How many snoozes it
/// made due.
pub(super) fn incoming(
    tx: &Transaction,
    account: i64,
    messages: &[&str],
    now: i64,
) -> Result<usize, StoreError> {
    if messages.is_empty() {
        return Ok(0);
    }
    Ok(tx.execute(INCOMING, params![account, json(&messages), now])?)
}

/// Rail archived, trashed or moved `threads` (or snoozes one again): a thread that came back from
/// a snooze leaves its woken row, so its rollup dates it by its mail again.
pub(super) fn left_inbox(
    tx: &Transaction,
    account: i64,
    action: &Action,
    threads: &[i64],
) -> Result<(), StoreError> {
    if !matches!(
        action,
        Action::Archive {} | Action::Trash {} | Action::Move { .. }
    ) {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM snoozes WHERE account_id = ?1 AND woken_at IS NOT NULL
           AND EXISTS (SELECT 1 FROM threads t WHERE t.account_id = snoozes.account_id
                       AND t.remote_id = snoozes.thread_remote
                       AND t.id IN (SELECT value FROM json_each(?2)))",
        params![account, json(&threads)],
    )?;
    Ok(())
}

/// A sync batch touched `threads`: one that came back from a snooze but is out of the inbox now
/// (archived elsewhere, or its return reverted) leaves its woken row, so the rollup dates it by
/// its mail again. Run after the waiting actions are applied again, so a return still on its way
/// keeps the thread in the inbox.
pub(super) fn settle_woken(
    tx: &Transaction,
    account: i64,
    threads: &BTreeSet<i64>,
) -> Result<(), StoreError> {
    if threads.is_empty() {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM snoozes WHERE account_id = ?1 AND woken_at IS NOT NULL
           AND EXISTS (SELECT 1 FROM threads t WHERE t.account_id = snoozes.account_id
                       AND t.remote_id = snoozes.thread_remote
                       AND t.id IN (SELECT value FROM json_each(?2))
                       AND NOT EXISTS (SELECT 1 FROM messages m
                                       JOIN message_folders mf ON mf.message_id = m.id
                                       JOIN folders f ON f.id = mf.folder_id
                                       WHERE m.thread_id = t.id AND f.kind = 'inbox'))",
        params![account, json(threads)],
    )?;
    Ok(())
}

fn forget_woken(tx: &Transaction, account: i64, remote: &str) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM snoozes WHERE account_id = ?1 AND thread_remote = ?2 AND woken_at IS NOT NULL",
        params![account, remote],
    )?;
    Ok(())
}

fn known_row(tx: &Transaction, token: &str) -> Result<Option<OutboxId>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT id FROM outbox WHERE client_token = ?1",
            [token],
            |r| r.get(0),
        )
        .optional()?)
}

/// The snoozes linked to an archive row: (snooze, account, thread's provider id).
fn linked(tx: &Transaction, row: OutboxId) -> Result<Vec<(i64, i64, String)>, StoreError> {
    let mut stmt =
        tx.prepare("SELECT id, account_id, thread_remote FROM snoozes WHERE archive_row = ?1")?;
    let rows = stmt.query_map([row], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn thread_remote(
    conn: &Connection,
    account: i64,
    thread: i64,
) -> Result<Option<String>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT remote_id FROM threads WHERE account_id = ?1 AND id = ?2",
            params![account, thread],
            |r| r.get(0),
        )
        .optional()?)
}

fn thread_id(conn: &Connection, account: i64, remote: &str) -> Result<Option<i64>, StoreError> {
    Ok(conn
        .query_row(
            "SELECT id FROM threads WHERE account_id = ?1 AND remote_id = ?2",
            params![account, remote],
            |r| r.get(0),
        )
        .optional()?)
}

fn in_inbox(conn: &Connection, thread: i64) -> Result<bool, StoreError> {
    Ok(conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM messages m JOIN message_folders mf ON mf.message_id = m.id
                        JOIN folders f ON f.id = mf.folder_id
                        WHERE m.thread_id = ?1 AND f.kind = 'inbox')",
        [thread],
        |r| r.get(0),
    )?)
}

impl Store {
    /// The account's snoozes, soonest first.
    ///
    /// # Errors
    /// The database's, as a `StoreError`.
    pub fn snoozes(&self, account_id: i64) -> Result<Vec<SnoozeRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "{ROW} WHERE s.account_id = ?1 ORDER BY s.until, s.id"
        ))?;
        let rows = stmt.query_map([account_id], read_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The snooze of a thread (the store's id), whatever its state.
    ///
    /// # Errors
    /// The database's, as a `StoreError`.
    pub fn snooze_of_thread(&self, thread_id: i64) -> Result<Option<SnoozeRow>, StoreError> {
        Ok(self
            .conn
            .query_row(
                &format!(
                    "{ROW} JOIN threads t ON t.account_id = s.account_id AND t.remote_id = s.thread_remote
                     WHERE t.id = ?1"
                ),
                [thread_id],
                read_row,
            )
            .optional()?)
    }

    /// When the account's next active snooze is due.
    ///
    /// # Errors
    /// The database's, as a `StoreError`.
    pub fn snooze_next_due(&self, account_id: i64) -> Result<Option<i64>, StoreError> {
        Ok(self.conn.query_row(
            "SELECT min(until) FROM snoozes
             WHERE account_id = ?1 AND archive_row IS NULL AND woken_at IS NULL",
            [account_id],
            |r| r.get(0),
        )?)
    }

    /// How many of `threads` (the account's) a snooze must refuse: no mail in the inbox and no
    /// snooze still waiting (a snoozed thread may take a new time).
    ///
    /// # Errors
    /// The database's, as a `StoreError`.
    pub fn unsnoozable(&self, account_id: i64, threads: &[i64]) -> Result<usize, StoreError> {
        let refused: i64 = self.conn.query_row(
            "SELECT count(*) FROM threads t
             WHERE t.account_id = ?1 AND t.id IN (SELECT value FROM json_each(?2))
               AND NOT EXISTS (SELECT 1 FROM messages m JOIN message_folders mf ON mf.message_id = m.id
                               JOIN folders f ON f.id = mf.folder_id
                               WHERE m.thread_id = t.id AND f.kind = 'inbox')
               AND NOT EXISTS (SELECT 1 FROM snoozes s WHERE s.account_id = t.account_id
                               AND s.thread_remote = t.remote_id AND s.woken_at IS NULL)",
            params![account_id, json(&threads)],
            |r| r.get(0),
        )?;
        Ok(usize::try_from(refused).unwrap_or(0))
    }
}
