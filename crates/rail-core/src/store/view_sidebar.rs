//! The sidebar's numbers: each account's inbox, every folder of every account, the merged
//! mailboxes, and one count per saved search. A sibling read module of `view.rs`, like
//! `view_saved` and `view_targets`, holding the types the sidebar is drawn from and the one
//! `Store` method that fills them, built on the same conditions the list reads with.

use std::collections::HashMap;

use rusqlite::params;

use super::filter::Filter;
use super::snoozes::HELD;
use super::view::{AccountFilter, IN_INBOX, MAX_SIDEBAR_FOLDERS, count, ids_json, int};
use super::{Store, StoreError};

/// An account's inbox: its threads and how many have unread mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountCounts {
    pub account_id: i64,
    pub threads: u64,
    pub unread: u64,
}

/// One folder's threads and how many have unread mail in it. `name` is the label's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderCount {
    pub folder_id: i64,
    pub account_id: i64,
    pub name: String,
    pub kind: String,
    pub threads: u64,
    pub unread: u64,
}

/// The merged mailboxes' numbers. Sent and Trash show none, so none is counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnifiedCounts {
    /// Threads with a starred message and an unread one.
    pub starred_unread: u64,
    /// Drafts as the Drafts folder lists them (local and provider), without its cap: the sum
    /// of `drafts_by_account`.
    pub drafts: u64,
    /// `(store account id, drafts)` for each account asked, in that order: what the Drafts
    /// pane lists for that account (its local drafts and the provider drafts none of them owns).
    pub drafts_by_account: Vec<(i64, u64)>,
    /// Threads a snooze holds: the sidebar shows the Snoozed mailbox while there are any.
    pub snoozed: u64,
}

/// The sidebar's numbers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SidebarCounts {
    /// One per account asked for, in that order.
    pub accounts: Vec<AccountCounts>,
    /// By account, then inbox, sent, drafts, archive, spam, trash, the user's labels, the
    /// provider's other labels; by name within each.
    pub folders: Vec<FolderCount>,
    /// Starred and Drafts under UNIFIED.
    pub unified: UnifiedCounts,
    /// One per saved search asked for, in that order; empty when counts are off.
    pub saved: Vec<u64>,
}

impl Store {
    /// Inbox counts for each of `accounts` (in that order), every folder's counts, the merged
    /// mailboxes' counts, and one unread count per saved search in `saved` (also in order).
    /// With `saved` empty no saved-search query runs at all.
    ///
    /// # Errors
    /// The store could not be read.
    pub fn sidebar_counts(
        &self,
        accounts: &[i64],
        saved: &[(Filter, AccountFilter)],
        utc_offset: i32,
    ) -> Result<SidebarCounts, StoreError> {
        let ids = ids_json(accounts);
        let drafts_by_account = accounts
            .iter()
            .map(|&id| Ok((id, self.drafts_count(&[id])?)))
            .collect::<Result<Vec<(i64, u64)>, StoreError>>()?;
        let drafts = drafts_by_account.iter().map(|(_, n)| n).sum();
        let sql = format!(
            "SELECT t.account_id, count(*), sum(t.unread_count > 0) FROM threads t
             WHERE t.account_id IN (SELECT value FROM json_each(?1)) AND {IN_INBOX}
             GROUP BY t.account_id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let found: HashMap<i64, (u64, u64)> = stmt
            .query_map(params![ids], |r| {
                Ok((r.get(0)?, (count(r.get(1)?), count(r.get(2)?))))
            })?
            .collect::<Result<_, _>>()?;
        let accounts = accounts
            .iter()
            .map(|&account_id| {
                let (threads, unread) = found.get(&account_id).copied().unwrap_or((0, 0));
                AccountCounts {
                    account_id,
                    threads,
                    unread,
                }
            })
            .collect();
        Ok(SidebarCounts {
            accounts,
            folders: self.folder_counts(&ids)?,
            unified: UnifiedCounts {
                starred_unread: self.starred_unread(&ids)?,
                drafts,
                drafts_by_account,
                snoozed: self.snoozed(&ids)?,
            },
            saved: self.saved_counts(&ids, saved, utc_offset)?,
        })
    }

    /// Threads a snooze holds in these accounts (`snoozes::HELD`).
    fn snoozed(&self, ids: &str) -> Result<u64, StoreError> {
        let n: i64 = self.conn.query_row(
            &format!(
                "SELECT count(*) FROM threads t
                 WHERE t.account_id IN (SELECT value FROM json_each(?1)) AND {HELD}"
            ),
            params![ids],
            |r| r.get(0),
        )?;
        Ok(count(n))
    }

    /// Threads of these accounts (a JSON id list) with a starred message and an unread one.
    fn starred_unread(&self, ids: &str) -> Result<u64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT count(*) FROM threads t
             WHERE t.account_id IN (SELECT value FROM json_each(?1))
               AND t.is_starred = 1 AND t.unread_count > 0",
            params![ids],
            |r| r.get(0),
        )?;
        Ok(count(n))
    }

    /// Every account's folders, at most `MAX_SIDEBAR_FOLDERS` of each: the inbox and the
    /// other system folders first, then labels by name.
    fn folder_counts(&self, ids: &str) -> Result<Vec<FolderCount>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT r.id, r.account_id, r.name, r.kind,
               (SELECT count(DISTINCT m.thread_id) FROM message_folders mf
                  JOIN messages m ON m.id = mf.message_id WHERE mf.folder_id = r.id),
               (SELECT count(DISTINCT m.thread_id) FROM message_folders mf
                  JOIN messages m ON m.id = mf.message_id
                  WHERE mf.folder_id = r.id AND m.is_read = 0)
             FROM (SELECT f.id, f.account_id, f.name, f.kind,
                     row_number() OVER (PARTITION BY f.account_id ORDER BY
                       CASE f.kind WHEN 'inbox' THEN 0 WHEN 'sent' THEN 1
                         WHEN 'drafts' THEN 2 WHEN 'archive' THEN 3 WHEN 'spam' THEN 4
                         WHEN 'trash' THEN 5 WHEN 'user' THEN 6 ELSE 7 END, f.name) AS place
                   FROM folders f
                   WHERE f.account_id IN (SELECT value FROM json_each(?1))) r
             WHERE r.place <= ?2
             ORDER BY r.account_id, r.place",
        )?;
        let rows = stmt.query_map(params![ids, int(MAX_SIDEBAR_FOLDERS)], |r| {
            Ok(FolderCount {
                folder_id: r.get(0)?,
                account_id: r.get(1)?,
                name: r.get(2)?,
                kind: r.get(3)?,
                threads: count(r.get(4)?),
                unread: count(r.get(5)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}
