//! What a bulk select reads: the threads a list query matches as keys and flags, and the sender
//! of one thread's newest message from someone other than the user. A sibling read module of
//! `view.rs`, built on the same `where_clause`, so a select-all and the drawn list can never
//! disagree on what matches.

use rusqlite::types::Value;
use rusqlite::{params, params_from_iter};

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

    /// The `From` of the thread's newest message someone else sent: a message in the account's
    /// Sent or Drafts, or from the account's own address, is the user's and is passed over, so a
    /// thread the user replied to last still names the correspondent. `None` when there is no
    /// such thread, or no message in it has a readable address that is not the user's (both
    /// providers drop an address-less `From`). Reads `from_json` only, never the bodies.
    ///
    /// # Errors
    /// `StoreError` when the query cannot run.
    pub fn newest_sender(&self, thread_id: i64) -> Result<Option<Mailbox>, StoreError> {
        let mut statement = self.conn().prepare(
            "SELECT m.from_json, a.address FROM messages m
             JOIN accounts a ON a.id = m.account_id
             WHERE m.thread_id = ?1 AND NOT EXISTS (
               SELECT 1 FROM message_folders mf JOIN folders f ON f.id = mf.folder_id
               WHERE mf.message_id = m.id AND f.kind IN ('sent', 'drafts'))
             ORDER BY m.internal_date DESC, m.id DESC",
        )?;
        let mut rows = statement.query(params![thread_id])?;
        while let Some(row) = rows.next()? {
            let (json, own): (String, String) = (row.get(0)?, row.get(1)?);
            // An unreadable list reads as empty, as the roll-up does.
            let sender = mailboxes(&json)
                .into_iter()
                .find(|m| !m.address.trim().is_empty());
            if let Some(sender) = sender.filter(|m| !m.address.trim().eq_ignore_ascii_case(&own)) {
                return Ok(Some(sender));
            }
        }
        Ok(None)
    }
}
