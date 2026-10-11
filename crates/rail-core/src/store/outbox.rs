//! The outbox: actions waiting for the worker, in one table. Every write runs in one
//! `IMMEDIATE` transaction on the writer's connection (`Store::outbox_write`); the reads serve
//! the worker, the operations and the status. Rows hold ids, flags and Rail's error words only.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, Row, Transaction, TransactionBehavior, params};

use super::apply_local::{self, Prior};
use super::{Store, StoreError, rollup};
use crate::ops::{Action, OutboxId};

/// Attempts after which a transiently failing row gives up.
pub const MAX_ATTEMPTS: u32 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxState {
    Pending,
    Inflight,
    Failed,
}

impl OutboxState {
    fn parse(text: &str) -> Self {
        match text {
            "inflight" => Self::Inflight,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

/// One row. `last_error` is Rail's words with the provider's text, never mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRow {
    pub id: OutboxId,
    pub account_id: i64,
    pub action: Action,
    pub thread_ids: Vec<i64>,
    pub state: OutboxState,
    pub attempts: u32,
    pub not_before: i64,
    pub last_error: Option<String>,
    pub(crate) reversal: Vec<Prior>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewAction {
    pub account_id: i64,
    pub action: Action,
    /// Checked by the caller: this account's threads, deduplicated.
    pub threads: Vec<i64>,
    pub not_before: i64,
    pub client_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Back to `pending` until `retry_at`, unless this was attempt `MAX_ATTEMPTS`.
    Transient { error: String, retry_at: i64 },
    /// `failed`, its local change reverted.
    Permanent(String),
}

/// One outbox write for the writer thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxWrite {
    Enqueue(NewAction),
    Claim {
        account_id: i64,
    },
    Complete {
        id: OutboxId,
    },
    Fail {
        id: OutboxId,
        failure: Failure,
    },
    /// Inflight back to pending, no attempt counted (the account needs a login).
    Release {
        id: OutboxId,
    },
    Undo {
        ids: Vec<OutboxId>,
    },
    Retry {
        id: OutboxId,
    },
    /// Every inflight row of the account back to pending (daemon start).
    Recover {
        account_id: i64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutboxOutcome {
    /// The row ids, and how many of the threads the store holds.
    Enqueued {
        rows: Vec<OutboxId>,
        applied: usize,
    },
    Claimed(Option<OutboxRow>),
    Done,
    Failed {
        gave_up: bool,
        row: Option<OutboxRow>,
    },
    Undone {
        undone: Vec<OutboxId>,
        too_late: Vec<OutboxId>,
    },
    Retried(bool),
    Recovered(usize),
}

const COLUMNS: &str =
    "id, account_id, payload, thread_ids, state, attempts, not_before, last_error, reversal";

fn read_row(r: &Row<'_>) -> rusqlite::Result<OutboxRow> {
    let json = |i: usize| -> rusqlite::Result<String> { r.get(i) };
    let action: Action = serde_json::from_str(&json(2)?).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(OutboxRow {
        id: r.get(0)?,
        account_id: r.get(1)?,
        action,
        // Rail's own JSON; an unreadable list (a hand-edited store) acts on nothing.
        thread_ids: serde_json::from_str(&json(3)?).unwrap_or_default(),
        state: OutboxState::parse(&json(4)?),
        attempts: r.get(5)?,
        not_before: r.get(6)?,
        last_error: r.get(7)?,
        reversal: serde_json::from_str(&json(8)?).unwrap_or_default(),
    })
}

fn one(tx: &Transaction, id: OutboxId) -> Result<Option<OutboxRow>, StoreError> {
    Ok(tx
        .query_row(
            &format!("SELECT {COLUMNS} FROM outbox WHERE id = ?1"),
            [id],
            read_row,
        )
        .optional()?)
}

fn json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".into())
}

/// Rows (live, and `also` whatever their state) whose threads meet `threads`, oldest first.
pub(super) fn live_rows_touching(
    tx: &Transaction,
    threads: &BTreeSet<i64>,
    also: &[OutboxId],
) -> Result<Vec<OutboxRow>, StoreError> {
    let mut stmt = tx.prepare(&format!(
        "SELECT {COLUMNS} FROM outbox o
         WHERE (o.state IN ('pending', 'inflight') OR o.id IN (SELECT value FROM json_each(?2)))
           AND EXISTS (SELECT 1 FROM json_each(o.thread_ids) t
                       WHERE t.value IN (SELECT value FROM json_each(?1)))
         ORDER BY o.id"
    ))?;
    let rows = stmt.query_map(params![json(threads), json(&also)], read_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Every row, whatever its state, whose threads meet `threads`.
pub(super) fn rows_touching(
    tx: &Transaction,
    threads: &BTreeSet<i64>,
) -> Result<Vec<OutboxRow>, StoreError> {
    let mut stmt = tx.prepare(&format!(
        "SELECT {COLUMNS} FROM outbox o WHERE EXISTS (SELECT 1 FROM json_each(o.thread_ids) t
           WHERE t.value IN (SELECT value FROM json_each(?1))) ORDER BY o.id"
    ))?;
    let rows = stmt.query_map(params![json(threads)], read_row)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub(super) fn save_threads_and_reversal(
    tx: &Transaction,
    id: OutboxId,
    threads: &[i64],
    reversal: &[Prior],
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE outbox SET thread_ids = ?2, reversal = ?3 WHERE id = ?1",
        params![id, json(&threads), json(&reversal)],
    )?;
    Ok(())
}

/// How many of `threads` the store holds for `account`.
fn held(tx: &Transaction, account: i64, threads: &[i64]) -> Result<usize, StoreError> {
    let n: i64 = tx.query_row(
        "SELECT count(*) FROM threads WHERE account_id = ?1 AND id IN (SELECT value FROM json_each(?2))",
        params![account, json(&threads)],
        |r| r.get(0),
    )?;
    Ok(usize::try_from(n).unwrap_or(0))
}

/// Add the row and apply its change locally, in the caller's transaction. A token already in
/// the outbox answers with that row and changes nothing.
pub(crate) fn enqueue(tx: &Transaction, new: &NewAction) -> Result<(OutboxId, usize), StoreError> {
    let applied = held(tx, new.account_id, &new.threads)?;
    let known: Option<OutboxId> = tx
        .query_row(
            "SELECT id FROM outbox WHERE client_token = ?1",
            [&new.client_token],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = known {
        return Ok((id, applied));
    }
    let reversal = apply_local::apply(tx, new.account_id, &new.action, &new.threads)?;
    tx.execute(
        "INSERT INTO outbox (account_id, kind, payload, state, client_token, not_before, thread_ids, reversal)
         VALUES (?1, ?2, ?3, 'pending', ?4, ?5, ?6, ?7)",
        params![
            new.account_id,
            new.action.kind_column(),
            json(&new.action),
            new.client_token,
            new.not_before,
            json(&new.threads),
            json(&reversal)
        ],
    )?;
    let id = tx.last_insert_rowid();
    rollup::refresh(tx, &new.threads.iter().copied().collect())?;
    Ok((id, applied))
}

/// The oldest due pending row of `account`, now inflight.
pub(crate) fn claim_next(
    tx: &Transaction,
    account: i64,
    now: i64,
) -> Result<Option<OutboxRow>, StoreError> {
    let id: Option<OutboxId> = tx
        .query_row(
            "UPDATE outbox SET state = 'inflight', inflight_at = ?2
             WHERE id = (SELECT id FROM outbox WHERE account_id = ?1 AND state = 'pending'
                         AND not_before <= ?2 ORDER BY not_before, id LIMIT 1)
             RETURNING id",
            params![account, now],
            |r| r.get(0),
        )
        .optional()?;
    match id {
        Some(id) => one(tx, id),
        None => Ok(None),
    }
}

pub(crate) fn complete(tx: &Transaction, id: OutboxId) -> Result<(), StoreError> {
    tx.execute("DELETE FROM outbox WHERE id = ?1", [id])?;
    Ok(())
}

pub(crate) fn release(tx: &Transaction, id: OutboxId) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE outbox SET state = 'pending', inflight_at = NULL WHERE id = ?1 AND state = 'inflight'",
        [id],
    )?;
    Ok(())
}

/// Record a failure: back to pending with the backoff, or (permanent, or the last attempt)
/// `failed` with its local change reverted. Whether it gave up, and the row as it is now.
pub(crate) fn fail(
    tx: &Transaction,
    id: OutboxId,
    failure: &Failure,
) -> Result<(bool, Option<OutboxRow>), StoreError> {
    let Some(row) = one(tx, id)? else {
        return Ok((false, None));
    };
    let attempts = row.attempts.saturating_add(1);
    let (error, retry_at) = match failure {
        Failure::Transient { error, retry_at } if attempts < MAX_ATTEMPTS => {
            (error.clone(), Some(*retry_at))
        }
        Failure::Transient { error, .. } => (
            format!("gave up after {MAX_ATTEMPTS} attempts: {error}"),
            None,
        ),
        Failure::Permanent(error) => (error.clone(), None),
    };
    match retry_at {
        Some(at) => {
            tx.execute(
                "UPDATE outbox SET state = 'pending', attempts = ?2, last_error = ?3, not_before = ?4,
                   inflight_at = NULL WHERE id = ?1",
                params![id, attempts, error, at],
            )?;
        }
        None => {
            apply_local::restack(tx, &row.thread_ids.iter().copied().collect(), &[id])?;
            tx.execute(
                "UPDATE outbox SET state = 'failed', attempts = ?2, last_error = ?3, inflight_at = NULL
                 WHERE id = ?1",
                params![id, attempts, error],
            )?;
        }
    }
    Ok((retry_at.is_none(), one(tx, id)?))
}

/// Remove the rows still pending and revert their change; the others are too late.
pub(crate) fn remove_pending(
    tx: &Transaction,
    ids: &[OutboxId],
) -> Result<(Vec<OutboxId>, Vec<OutboxId>), StoreError> {
    let (mut undone, mut too_late, mut threads) = (Vec::new(), Vec::new(), BTreeSet::new());
    for &id in ids {
        match one(tx, id)? {
            Some(row) if row.state == OutboxState::Pending => {
                threads.extend(row.thread_ids.iter().copied());
                undone.push(id);
            }
            _ => too_late.push(id),
        }
    }
    apply_local::restack(tx, &threads, &undone)?;
    for id in &undone {
        complete(tx, *id)?;
    }
    Ok((undone, too_late))
}

/// `failed` → `pending` now, its change applied again in order. False for any other row.
pub(crate) fn retry(tx: &Transaction, id: OutboxId, now: i64) -> Result<bool, StoreError> {
    let Some(row) = one(tx, id)?.filter(|r| r.state == OutboxState::Failed) else {
        return Ok(false);
    };
    tx.execute(
        "UPDATE outbox SET state = 'pending', attempts = 0, last_error = NULL, not_before = ?2,
           reversal = '[]' WHERE id = ?1",
        params![id, now],
    )?;
    apply_local::restack(tx, &row.thread_ids.iter().copied().collect(), &[])?;
    Ok(true)
}

/// The restart rule: inflight rows of `account` are sent again.
pub(crate) fn recover(tx: &Transaction, account: i64) -> Result<usize, StoreError> {
    Ok(tx.execute(
        "UPDATE outbox SET state = 'pending', inflight_at = NULL WHERE account_id = ?1 AND state = 'inflight'",
        [account],
    )?)
}

impl Store {
    /// One outbox write in one `IMMEDIATE` transaction.
    pub fn outbox_write(
        &mut self,
        write: &OutboxWrite,
        now: i64,
    ) -> Result<OutboxOutcome, StoreError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let outcome = match write {
            OutboxWrite::Enqueue(new) => {
                let (id, applied) = enqueue(&tx, new)?;
                OutboxOutcome::Enqueued {
                    rows: vec![id],
                    applied,
                }
            }
            OutboxWrite::Claim { account_id } => {
                OutboxOutcome::Claimed(claim_next(&tx, *account_id, now)?)
            }
            OutboxWrite::Complete { id } => {
                complete(&tx, *id)?;
                OutboxOutcome::Done
            }
            OutboxWrite::Fail { id, failure } => {
                let (gave_up, row) = fail(&tx, *id, failure)?;
                OutboxOutcome::Failed { gave_up, row }
            }
            OutboxWrite::Release { id } => {
                release(&tx, *id)?;
                OutboxOutcome::Done
            }
            OutboxWrite::Undo { ids } => {
                let (undone, too_late) = remove_pending(&tx, ids)?;
                OutboxOutcome::Undone { undone, too_late }
            }
            OutboxWrite::Retry { id } => OutboxOutcome::Retried(retry(&tx, *id, now)?),
            OutboxWrite::Recover { account_id } => {
                OutboxOutcome::Recovered(recover(&tx, *account_id)?)
            }
        };
        tx.commit()?;
        Ok(outcome)
    }

    /// `(waiting, failed)` rows of the account; waiting counts inflight ones.
    pub fn outbox_depth(&self, account_id: i64) -> Result<(u64, u64), StoreError> {
        let (waiting, failed): (i64, i64) = self.conn.query_row(
            "SELECT COALESCE(sum(state != 'failed'), 0), COALESCE(sum(state = 'failed'), 0)
             FROM outbox WHERE account_id = ?1",
            [account_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((
            u64::try_from(waiting).unwrap_or(0),
            u64::try_from(failed).unwrap_or(0),
        ))
    }

    /// The earliest `not_before` of a pending row.
    pub fn outbox_next_due(&self, account_id: i64) -> Result<Option<i64>, StoreError> {
        Ok(self.conn.query_row(
            "SELECT min(not_before) FROM outbox WHERE account_id = ?1 AND state = 'pending'",
            [account_id],
            |r| r.get(0),
        )?)
    }

    /// Live rows whose threads meet `thread_ids`.
    pub fn pending_for(&self, thread_ids: &[i64]) -> Result<Vec<OutboxRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM outbox o WHERE o.state IN ('pending', 'inflight')
             AND EXISTS (SELECT 1 FROM json_each(o.thread_ids) t
                         WHERE t.value IN (SELECT value FROM json_each(?1))) ORDER BY o.id"
        ))?;
        let rows = stmt.query_map(params![json(&thread_ids)], read_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn failed_rows(&self, account_id: i64) -> Result<Vec<OutboxRow>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM outbox WHERE account_id = ?1 AND state = 'failed' ORDER BY id"
        ))?;
        let rows = stmt.query_map([account_id], read_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The provider ids of the threads of `threads` the store still holds for the account.
    pub fn thread_remotes(
        &self,
        account_id: i64,
        threads: &[i64],
    ) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT remote_id FROM threads WHERE account_id = ?1
             AND id IN (SELECT value FROM json_each(?2)) ORDER BY id",
        )?;
        let rows = stmt.query_map(params![account_id, json(&threads)], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The provider id of one of the account's folders.
    pub fn folder_remote(
        &self,
        account_id: i64,
        folder_id: i64,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT remote_id FROM folders WHERE account_id = ?1 AND id = ?2",
                params![account_id, folder_id],
                |r| r.get(0),
            )
            .optional()?)
    }
}
