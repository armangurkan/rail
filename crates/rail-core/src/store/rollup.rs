//! Thread rollups, recomputed only for the threads a batch touched: counts,
//! flags, the newest receive time, the earliest message's subject and the distinct senders.

use std::collections::BTreeSet;

use rusqlite::{Transaction, params};

use super::StoreError;
use crate::provider::Mailbox;

const MAX_PARTICIPANTS: usize = 10;

const ROLLUP: &str = "UPDATE threads SET
    message_count = (SELECT count(*) FROM messages WHERE thread_id = ?1),
    unread_count = (SELECT count(*) FROM messages WHERE thread_id = ?1 AND is_read = 0),
    is_starred = EXISTS (SELECT 1 FROM messages WHERE thread_id = ?1 AND is_starred = 1),
    has_attachments = EXISTS (SELECT 1 FROM attachments a JOIN messages m ON m.id = a.message_id
                              WHERE m.thread_id = ?1),
    last_date = max(COALESCE((SELECT max(internal_date) FROM messages WHERE thread_id = ?1), 0) / 1000,
                    COALESCE((SELECT s.woken_at FROM snoozes s JOIN threads w
                              ON w.account_id = s.account_id AND w.remote_id = s.thread_remote
                              WHERE w.id = ?1 AND s.woken_at IS NOT NULL), 0)),
    subject = COALESCE((SELECT subject FROM messages WHERE thread_id = ?1
                        ORDER BY internal_date LIMIT 1), ''),
    participants = ?2
  WHERE id = ?1";

pub(super) fn refresh(tx: &Transaction, threads: &BTreeSet<i64>) -> Result<(), StoreError> {
    for &thread in threads {
        let participants = participants(tx, thread)?;
        tx.execute(ROLLUP, params![thread, participants])?;
        // A thread with no mail left takes its snooze with it: snoozes are keyed by the
        // provider's thread id, which a later thread may reuse.
        tx.execute(
            "DELETE FROM snoozes WHERE (account_id, thread_remote) IN
               (SELECT account_id, remote_id FROM threads WHERE id = ?1 AND message_count = 0)",
            params![thread],
        )?;
        tx.execute(
            "DELETE FROM threads WHERE id = ?1 AND message_count = 0",
            params![thread],
        )?;
    }
    Ok(())
}

/// The distinct senders of a thread in date order, at most `MAX_PARTICIPANTS`, as JSON.
fn participants(tx: &Transaction, thread: i64) -> Result<String, StoreError> {
    let mut stmt =
        tx.prepare("SELECT from_json FROM messages WHERE thread_id = ?1 ORDER BY internal_date")?;
    let mut seen: Vec<Mailbox> = Vec::new();
    for row in stmt.query_map(params![thread], |r| r.get::<_, String>(0))? {
        // Rows the writer wrote from `Mailbox` lists; an unreadable one adds no one.
        let senders: Vec<Mailbox> = serde_json::from_str(&row?).unwrap_or_default();
        for sender in senders {
            let known = seen
                .iter()
                .any(|p| p.address.eq_ignore_ascii_case(&sender.address));
            if !known && seen.len() < MAX_PARTICIPANTS {
                seen.push(sender);
            }
        }
    }
    Ok(serde_json::to_string(&seen).unwrap_or_else(|_| "[]".to_string()))
}
