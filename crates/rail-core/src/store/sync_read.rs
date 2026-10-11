//! What the sync engine and search read. Every query here only reads; the writer
//! (`sync::writer`) is the only code that changes sync data.

use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, params};

use super::{Store, StoreError};
use crate::provider::{PageToken, Window};
use crate::sync::batch::ResumeToken;

/// One account's sync columns.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncStateRow {
    pub account_id: i64,
    /// Gmail's `historyId` the next delta starts from.
    pub cursor: Option<String>,
    pub backfill_before: Option<i64>,
    pub backfill_cursor: Option<String>,
    pub backfill_window: Option<i64>,
    pub backfill_started_at: Option<i64>,
    /// Where the running backfill's listing goes on. A stored token Rail would not send is
    /// read as none (the backfill then lists the window from the top).
    pub backfill_resume: Option<ResumeToken>,
    pub needs_auth: bool,
    pub phase: String,
    pub last_sync_at: Option<i64>,
    pub last_error_code: Option<String>,
    pub last_error_at: Option<i64>,
}

/// A stored message a search found. `subject` and `from_json` are mail content: outside text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredHit {
    pub account: String,
    pub remote_id: String,
    pub thread_remote_id: String,
    pub subject: String,
    pub from_json: String,
    pub internal_date: i64,
    pub origin: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StoreCounts {
    pub messages: u64,
    pub threads: u64,
    pub bodies: u64,
}

/// The user's words as an FTS5 query that cannot be a syntax error: each word becomes a quoted
/// string (inner quotes doubled), so operators and brackets are matched as text; words without
/// a letter or digit are dropped. `None` when no word is left.
pub fn fts_query(text: &str) -> Option<String> {
    let words: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c.is_control())
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .map(|w| format!("\"{}\"", w.replace('"', "\"\"")))
        .collect();
    (!words.is_empty()).then(|| words.join(" "))
}

const HIT_COLUMNS: &str =
    "a.name, m.remote_id, t.remote_id, m.subject, m.from_json, m.internal_date, m.origin
     FROM messages m JOIN threads t ON t.id = m.thread_id JOIN accounts a ON a.id = m.account_id";

fn hit(r: &rusqlite::Row<'_>) -> rusqlite::Result<StoredHit> {
    Ok(StoredHit {
        account: r.get(0)?,
        remote_id: r.get(1)?,
        thread_remote_id: r.get(2)?,
        subject: r.get(3)?,
        from_json: r.get(4)?,
        internal_date: r.get(5)?,
        origin: r.get(6)?,
    })
}

/// The stored resume token (columns 10 to 13), if there is one Rail would send.
fn resume(r: &rusqlite::Row<'_>) -> rusqlite::Result<Option<ResumeToken>> {
    let token: Option<String> = r.get(10)?;
    let issued_at: Option<i64> = r.get(13)?;
    let (Some(token), Some(issued_at)) = (token, issued_at) else {
        return Ok(None);
    };
    let listing = Window {
        after_ms: r.get(11)?,
        before_ms: r.get(12)?,
    };
    Ok(PageToken::checked(&token).map(|token| ResumeToken {
        token,
        listing,
        issued_at,
    }))
}

fn count(store: &Store, sql: &str, account_id: i64) -> Result<u64, StoreError> {
    let n: i64 = store
        .conn
        .query_row(sql, params![account_id], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

impl Store {
    /// The store id of the account with this address (stored lowercase).
    pub fn account_id(&self, address: &str) -> Result<Option<i64>, StoreError> {
        Ok(self
            .conn
            .query_row(
                "SELECT id FROM accounts WHERE address = ?1",
                params![address.to_ascii_lowercase()],
                |r| r.get(0),
            )
            .optional()?)
    }

    pub fn sync_state(&self, account_id: i64) -> Result<SyncStateRow, StoreError> {
        Ok(self.conn.query_row(
            "SELECT sync_cursor, backfill_before, backfill_cursor, backfill_window,
                    backfill_started_at, needs_auth, sync_phase, last_sync_at,
                    last_error_code, last_error_at, backfill_page_token,
                    backfill_list_after_ms, backfill_list_before_ms, backfill_token_at
             FROM accounts WHERE id = ?1",
            params![account_id],
            |r| {
                Ok(SyncStateRow {
                    account_id,
                    cursor: r.get(0)?,
                    backfill_before: r.get(1)?,
                    backfill_cursor: r.get(2)?,
                    backfill_window: r.get(3)?,
                    backfill_started_at: r.get(4)?,
                    needs_auth: r.get::<_, i64>(5)? != 0,
                    phase: r.get(6)?,
                    last_sync_at: r.get(7)?,
                    last_error_code: r.get(8)?,
                    last_error_at: r.get(9)?,
                    backfill_resume: resume(r)?,
                })
            },
        )?)
    }

    /// How many messages `backfilled_ids` would match inside `listing`'s receive times (in
    /// milliseconds; Gmail compares whole seconds, so this may count a few more): the most
    /// pages of stored mail a re-walked listing can hold. One indexed query.
    pub fn backfilled_count(
        &self,
        account_id: i64,
        listing: &Window,
        since: Option<i64>,
    ) -> Result<u64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM messages
             WHERE account_id = ?1 AND origin = 'sync'
               AND (?2 IS NULL OR internal_date >= ?2) AND (?3 IS NULL OR internal_date < ?3)
               AND (?4 IS NULL OR synced_at >= ?4)",
            params![account_id, listing.after_ms, listing.before_ms, since],
            |r| r.get(0),
        )?;
        Ok(u64::try_from(n).unwrap_or(0))
    }

    /// Which of `ids` (one listing page) a running backfill already stored for the account:
    /// `origin = sync` rows synced at or after `since` (any time when `None`). One query.
    pub fn backfilled_ids(
        &self,
        account_id: i64,
        ids: &[String],
        since: Option<i64>,
    ) -> Result<HashSet<String>, StoreError> {
        if ids.is_empty() {
            return Ok(HashSet::new());
        }
        let mut stmt = self.conn.prepare(
            "SELECT remote_id FROM messages
             WHERE account_id = ?1 AND origin = 'sync'
               AND remote_id IN (SELECT value FROM json_each(?2))
               AND (?3 IS NULL OR synced_at >= ?3)",
        )?;
        let ids = serde_json::Value::from(ids.to_vec()).to_string();
        let rows = stmt.query_map(params![account_id, ids, since], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Messages without a body in threads that have a message received since `since_ms`,
    /// newest first, at most `limit`.
    pub fn messages_wanting_bodies(
        &self,
        account_id: i64,
        since_ms: i64,
        limit: usize,
    ) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT m.remote_id FROM messages m
             WHERE m.account_id = ?1 AND m.body_fetched_at IS NULL
               AND m.thread_id IN (SELECT thread_id FROM messages
                                   WHERE account_id = ?1 AND internal_date >= ?2)
             ORDER BY m.internal_date DESC LIMIT ?3",
        )?;
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let rows = stmt.query_map(params![account_id, since_ms, limit], |r| r.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Which of `ids` the store already holds for the account.
    pub fn known_remote_ids(
        &self,
        account_id: i64,
        ids: &[String],
    ) -> Result<HashSet<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT 1 FROM messages WHERE account_id = ?1 AND remote_id = ?2")?;
        let mut known = HashSet::new();
        for id in ids {
            if stmt.exists(params![account_id, id])? {
                known.insert(id.clone());
            }
        }
        Ok(known)
    }

    pub fn counts(&self, account_id: i64) -> Result<StoreCounts, StoreError> {
        Ok(StoreCounts {
            messages: count(
                self,
                "SELECT count(*) FROM messages WHERE account_id = ?1",
                account_id,
            )?,
            threads: count(
                self,
                "SELECT count(*) FROM threads WHERE account_id = ?1",
                account_id,
            )?,
            bodies: count(
                self,
                "SELECT count(*) FROM messages WHERE account_id = ?1 AND body_fetched_at IS NOT NULL",
                account_id,
            )?,
        })
    }

    /// Full-text search over the store, newest first. `text` is the user's words, quoted by
    /// `fts_query`; nothing matches when it has no words. `accounts` limits it to those
    /// account ids (`None`: every account in the store).
    pub fn search_local(
        &self,
        text: &str,
        accounts: Option<&[i64]>,
        limit: usize,
    ) -> Result<Vec<StoredHit>, StoreError> {
        let Some(query) = fts_query(text) else {
            return Ok(Vec::new());
        };
        let sql = format!(
            "SELECT {HIT_COLUMNS} JOIN messages_fts f ON f.rowid = m.id
             WHERE messages_fts MATCH ?1
               AND (?2 IS NULL OR m.account_id IN (SELECT value FROM json_each(?2)))
             ORDER BY m.internal_date DESC, m.id DESC LIMIT ?3"
        );
        let accounts = accounts.map(|ids| serde_json::Value::from(ids.to_vec()).to_string());
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params![query, accounts, limit], hit)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The stored messages among `remote_ids`, in no particular order.
    pub fn hits_for(
        &self,
        account_id: i64,
        remote_ids: &[String],
    ) -> Result<Vec<StoredHit>, StoreError> {
        let sql = format!("SELECT {HIT_COLUMNS} WHERE m.account_id = ?1 AND m.remote_id = ?2");
        let mut stmt = self.conn.prepare(&sql)?;
        let mut hits = Vec::new();
        for id in remote_ids {
            if let Some(found) = stmt.query_row(params![account_id, id], hit).optional()? {
                hits.push(found);
            }
        }
        Ok(hits)
    }

    /// The body state of each of `ids` the store holds for the account; ids it does not hold
    /// are left out. One query.
    pub fn body_states(
        &self,
        account_id: i64,
        ids: &[String],
    ) -> Result<HashMap<String, StoredBody>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT remote_id, body_fetched_at IS NOT NULL, body_text = ?3 FROM messages
             WHERE account_id = ?1 AND remote_id IN (SELECT value FROM json_each(?2))",
        )?;
        let ids = serde_json::Value::from(ids.to_vec()).to_string();
        let rows = stmt.query_map(
            params![account_id, ids, crate::sync::UNREADABLE_BODY],
            |r| {
                let state = match (r.get::<_, bool>(1)?, r.get::<_, Option<bool>>(2)?) {
                    (false, _) => StoredBody::Pending,
                    (true, Some(true)) => StoredBody::Unreadable,
                    (true, _) => StoredBody::Stored,
                };
                Ok((r.get::<_, String>(0)?, state))
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

/// What the store holds of one message's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredBody {
    Stored,
    /// The `UNREADABLE_BODY` marker.
    Unreadable,
    /// Not fetched yet.
    Pending,
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 86_400_000;

    /// Account 1 (`a@example.com`) with threads t1 (m1 old, m2 recent, no bodies) and t2 (m3 old).
    fn seeded() -> Store {
        let store = Store::open_in_memory().unwrap();
        store
            .conn()
            .execute_batch(&format!(
                "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', 'a@example.com');
                 INSERT INTO threads (account_id, remote_id) VALUES (1, 't1'), (1, 't2');
                 INSERT INTO messages (account_id, thread_id, remote_id, subject, from_json, internal_date)
                   VALUES (1, 1, 'm1', 'quarterly NEAR roadmap (draft)', '[{{\"name\":\"Dana\",\"address\":\"d@x.com\"}}]', {old}),
                          (1, 1, 'm2', 're: roadmap', '[]', {recent}),
                          (1, 2, 'm3', 'lunch', '[]', {old});",
                old = 100 * DAY_MS,
                recent = 130 * DAY_MS,
            ))
            .unwrap();
        store
    }

    #[test]
    fn fts_query_quotes_every_term() {
        assert_eq!(
            fts_query("say \"hi\" -x NEAR(a b) * AND (").as_deref(),
            Some("\"say\" \"\"\"hi\"\"\" \"-x\" \"NEAR(a\" \"b)\" \"AND\"")
        );
        assert_eq!(fts_query("  * ( ) "), None);
        assert_eq!(fts_query(""), None);
    }

    /// Review focus 5.
    #[test]
    fn local_search_survives_fts_syntax() {
        let store = seeded();
        for hostile in [
            "\"",
            "roadmap\"",
            "NEAR(",
            "a AND",
            "-roadmap",
            "(roadmap",
            "*",
            "OR OR",
        ] {
            store.search_local(hostile, None, 10).unwrap();
        }
        let hits = store.search_local("roadmap (draft", None, 10).unwrap();
        let ids: Vec<&str> = hits.iter().map(|h| h.remote_id.as_str()).collect();
        assert_eq!(ids, ["m1"]);
        assert_eq!(hits[0].account, "work");
        assert_eq!(hits[0].thread_remote_id, "t1");
        let both = store.search_local("roadmap", Some(&[1]), 10).unwrap();
        let ids: Vec<&str> = both.iter().map(|h| h.remote_id.as_str()).collect();
        assert_eq!(ids, ["m2", "m1"], "newest first");
        assert!(
            store
                .search_local("roadmap", Some(&[2]), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn nul_and_control_characters_split_words() {
        assert_eq!(fts_query("ab\0cd").as_deref(), Some("\"ab\" \"cd\""));
        assert_eq!(fts_query("\0"), None);
    }

    #[test]
    fn a_new_account_has_no_sync_state() {
        let store = seeded();
        assert_eq!(store.account_id("A@Example.com").unwrap(), Some(1));
        assert_eq!(store.account_id("nobody@example.com").unwrap(), None);
        let state = store.sync_state(1).unwrap();
        assert_eq!(
            state,
            SyncStateRow {
                account_id: 1,
                phase: "idle".into(),
                ..SyncStateRow::default()
            }
        );
    }

    #[test]
    fn bodies_are_wanted_for_every_message_of_a_recent_thread() {
        let store = seeded();
        let wanted = store.messages_wanting_bodies(1, 120 * DAY_MS, 10).unwrap();
        assert_eq!(wanted, ["m2", "m1"], "t1 has a recent message; t2 has none");
        store
            .conn()
            .execute(
                "UPDATE messages SET body_fetched_at = 1 WHERE remote_id = 'm1'",
                [],
            )
            .unwrap();
        assert_eq!(
            store.messages_wanting_bodies(1, 120 * DAY_MS, 10).unwrap(),
            ["m2"]
        );
        assert_eq!(
            store
                .messages_wanting_bodies(1, 120 * DAY_MS, 0)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn known_ids_counts_and_hits() {
        let store = seeded();
        let ids = ["m1".to_string(), "zz".to_string(), "m3".to_string()];
        let known = store.known_remote_ids(1, &ids).unwrap();
        assert_eq!(known.len(), 2);
        assert!(known.contains("m1") && known.contains("m3"));
        assert_eq!(
            store.counts(1).unwrap(),
            StoreCounts {
                messages: 3,
                threads: 2,
                bodies: 0
            }
        );
        let hits = store.hits_for(1, &ids).unwrap();
        let mut found: Vec<&str> = hits.iter().map(|h| h.remote_id.as_str()).collect();
        found.sort_unstable();
        assert_eq!(found, ["m1", "m3"]);
    }

    #[test]
    fn body_states_tell_stored_unreadable_and_pending_apart() {
        let store = seeded();
        store
            .conn()
            .execute_batch(&format!(
                "UPDATE messages SET body_text = 'x', body_fetched_at = 1 WHERE remote_id = 'm1';
                 UPDATE messages SET body_text = '{}', body_fetched_at = 1 WHERE remote_id = 'm2';",
                crate::sync::UNREADABLE_BODY.replace('\'', "''")
            ))
            .unwrap();
        let ids: Vec<String> = ["m1", "m2", "m3", "zz"].map(String::from).to_vec();
        let states = store.body_states(1, &ids).unwrap();
        assert_eq!(states.get("m1"), Some(&StoredBody::Stored));
        assert_eq!(states.get("m2"), Some(&StoredBody::Unreadable));
        assert_eq!(states.get("m3"), Some(&StoredBody::Pending));
        assert_eq!(states.get("zz"), None);
        assert!(
            store.body_states(2, &ids).unwrap().is_empty(),
            "another account's ids"
        );
    }
}
