//! What a bulk select reads: the threads a list query matches as keys and flags, and the sender
//! of one thread's newest message. A sibling read module of `view.rs`, built on the same
//! `where_clause`, so a select-all and the drawn list can never disagree on what matches.

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, params, params_from_iter};

use super::view::{AccountFilter, ListQuery, ThreadKey, int, mailboxes, where_clause};
use super::{Store, StoreError};
use crate::provider::Mailbox;

/// The select-all's `SELECT` over the `where_clause` text `base`, and its parameters in order.
/// Mirrors `view::rows_statement` so a unit test can read the projection: the five scalar
/// columns a selection needs and nothing else — no subject, no participants, no body.
///
/// `is_starred` and `unread_count` are the rollup columns a drawn row reads too
/// (`selection.rs`'s `Picked::of` computes `unread_count > 0`), so the flags a bulk select
/// carries cannot disagree with the flags the list showed.
pub(super) fn targets_statement(
    base: &str,
    params: &[Value],
    limit: usize,
) -> (String, Vec<Value>) {
    let mut values = params.to_vec();
    values.push(int(limit));
    let sql = format!(
        "SELECT t.last_date, t.id, t.account_id, t.is_starred, t.unread_count > 0
         FROM threads t WHERE {base}
         ORDER BY t.last_date DESC, t.id DESC LIMIT ?"
    );
    (sql, values)
}

impl Store {
    /// The threads `query` matches, newest first, at most `limit`: the key, the account, and
    /// whether the thread is starred and has unread mail — never a subject or a participant, so
    /// a 10,000-thread selection carries no mail content.
    ///
    /// # Errors
    /// `StoreError` when the query cannot run.
    pub fn matching_targets(
        &self,
        query: &ListQuery,
        limit: usize,
    ) -> Result<Vec<(ThreadKey, i64, bool, bool)>, StoreError> {
        if query.account == AccountFilter::Nobody {
            return Ok(Vec::new());
        }
        let (base, params) = where_clause(query);
        let (sql, values) = targets_statement(&base, &params, limit);
        let mut statement = self.conn().prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), |row| {
            Ok((
                ThreadKey {
                    last_date: row.get(0)?,
                    thread_id: row.get(1)?,
                },
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The `From` of the thread's newest message; `None` when there is no such thread, or its
    /// newest message carries no readable address (both providers drop an address-less `From`).
    /// One row: never `thread_detail`, which loads up to 200 messages with their bodies.
    ///
    /// # Errors
    /// `StoreError` when the query cannot run.
    pub fn newest_sender(&self, thread_id: i64) -> Result<Option<Mailbox>, StoreError> {
        let from: Option<String> = self
            .conn()
            .query_row(
                "SELECT from_json FROM messages WHERE thread_id = ?1
                 ORDER BY internal_date DESC, id DESC LIMIT 1",
                params![thread_id],
                |row| row.get(0),
            )
            .optional()?;
        let Some(json) = from else {
            return Ok(None);
        };
        // An unreadable list reads as empty, as the roll-up does.
        let sender = mailboxes(&json)
            .into_iter()
            .find(|m| !m.address.trim().is_empty());
        Ok(sender)
    }
}
