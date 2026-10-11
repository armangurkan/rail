//! `users.history.list` as a `ChangeSet`. A 404 (an out-of-date `startHistoryId`) is
//! `CursorExpired`. At most `MAX_HISTORY_PAGES` pages are read per call; then `more` is set
//! and the cursor is the last record read, so the engine commits and calls again. Every id a
//! record carries is checked before it is used, and each page, record and label list is capped.

use std::collections::{HashMap, HashSet};

use super::labels::{STARRED, UNREAD};
use super::wire::{History, HistoryLabels, HistoryList, HistoryMessage};
use super::{COST_HISTORY, GmailProvider, MAX_HISTORY_ID_DIGITS, checked_id};
use crate::provider::{ChangeSet, ProviderError, Relabel, RemoteId, SyncCursor};

/// History pages read per call.
pub const MAX_HISTORY_PAGES: usize = 10;
/// History records read from one page (the page size Rail asks for); a page carrying more is
/// cut there and the next call resumes after the last record kept.
pub const MAX_HISTORY_RECORDS: usize = 500;
/// Messages (or label changes) kept from one record.
const MAX_RECORD_ITEMS: usize = 100;
/// Label ids kept from one label change.
const MAX_LABELS: usize = 100;
const HISTORY_TYPES: [&str; 4] = [
    "messageAdded",
    "messageDeleted",
    "labelAdded",
    "labelRemoved",
];

/// A `historyId` Rail can send back: 1 to 20 decimal digits that fit a `u64`.
fn history_number(id: &str) -> Option<u64> {
    if id.is_empty() || id.len() > MAX_HISTORY_ID_DIGITS || !id.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    id.parse().ok()
}

/// The safe message ids of `items`, at most `MAX_RECORD_ITEMS`.
fn message_ids(items: Vec<HistoryMessage>) -> impl Iterator<Item = RemoteId> {
    items
        .into_iter()
        .map(|item| item.message.id)
        .filter(|id| checked_id(id).is_some())
        .take(MAX_RECORD_ITEMS)
}

/// The label changes of `items` whose message id is safe, each with only its safe label ids.
fn label_changes(items: Vec<HistoryLabels>) -> impl Iterator<Item = (RemoteId, Vec<String>)> {
    items
        .into_iter()
        .filter(|item| checked_id(&item.message.id).is_some())
        .take(MAX_RECORD_ITEMS)
        .map(|item| {
            let labels = item
                .label_ids
                .into_iter()
                .filter(|l| checked_id(l).is_some())
                .take(MAX_LABELS)
                .collect();
            (item.message.id, labels)
        })
}

/// The net effect of history records, applied in order.
#[derive(Default)]
struct Net {
    added: Vec<RemoteId>,
    added_set: HashSet<RemoteId>,
    deleted: Vec<RemoteId>,
    deleted_set: HashSet<RemoteId>,
    relabels: HashMap<RemoteId, Relabel>,
    relabel_order: Vec<RemoteId>,
}

impl Net {
    fn apply(&mut self, record: History) {
        for id in message_ids(record.messages_added) {
            if !self.deleted_set.contains(&id) && self.added_set.insert(id.clone()) {
                self.added.push(id);
            }
        }
        for id in message_ids(record.messages_deleted) {
            if self.added_set.remove(&id) {
                self.added.retain(|a| *a != id);
            }
            self.relabels.remove(&id);
            if self.deleted_set.insert(id.clone()) {
                self.deleted.push(id);
            }
        }
        for (id, labels) in label_changes(record.labels_added) {
            self.relabel(id, &labels, true);
        }
        for (id, labels) in label_changes(record.labels_removed) {
            self.relabel(id, &labels, false);
        }
    }

    /// A label change for a message the set adds or deletes is already covered.
    fn relabel(&mut self, id: RemoteId, labels: &[String], added: bool) {
        if self.deleted_set.contains(&id) || self.added_set.contains(&id) {
            return;
        }
        if !self.relabels.contains_key(&id) {
            self.relabel_order.push(id.clone());
        }
        let change = self.relabels.entry(id.clone()).or_insert_with(|| Relabel {
            remote_id: id,
            ..Relabel::default()
        });
        for label in labels {
            match label.as_str() {
                UNREAD => change.read = Some(!added),
                STARRED => change.starred = Some(added),
                _ if added => {
                    change.remove_folders.retain(|f| f != label);
                    if !change.add_folders.contains(label) {
                        change.add_folders.push(label.clone());
                    }
                }
                _ => {
                    change.add_folders.retain(|f| f != label);
                    if !change.remove_folders.contains(label) {
                        change.remove_folders.push(label.clone());
                    }
                }
            }
        }
    }
}

/// Where reading stopped early, and whether to call again.
struct Progress {
    /// The last record read whose id Rail can send back.
    last_record: Option<String>,
    /// The mailbox's history id the latest page reported.
    latest: Option<String>,
}

impl Progress {
    /// The cursor and `more` for a call that stopped before Gmail's last page. `more` is only
    /// set when the cursor moved past `start`: a call never asks to be repeated from where it
    /// began. With no record to resume after, the mailbox's history id is the cursor.
    fn resume(self, start: &SyncCursor) -> (SyncCursor, bool) {
        let begun = history_number(&start.0);
        let advanced = self.last_record.filter(|r| history_number(r) > begun);
        if let Some(record) = advanced {
            return (SyncCursor(record), true);
        }
        tracing::warn!(
            "Gmail's history pages carried no records to resume after; \
             moving the cursor to the mailbox's current history id"
        );
        (self.latest.map_or_else(|| start.clone(), SyncCursor), false)
    }
}

impl GmailProvider {
    pub(crate) async fn history(&self, cursor: &SyncCursor) -> Result<ChangeSet, ProviderError> {
        if history_number(&cursor.0).is_none() {
            return Err(ProviderError::CursorExpired);
        }
        let mut net = Net::default();
        let mut page: Option<String> = None;
        let mut progress = Progress {
            last_record: None,
            latest: None,
        };
        for _ in 0..MAX_HISTORY_PAGES {
            let list = self.history_page(cursor, page.as_deref()).await?;
            if let Some(id) = list.history_id.filter(|id| history_number(id).is_some()) {
                progress.latest = Some(id);
            }
            let cut = list.history.len() > MAX_HISTORY_RECORDS;
            for record in list.history.into_iter().take(MAX_HISTORY_RECORDS) {
                if history_number(&record.id).is_some() {
                    progress.last_record = Some(record.id.clone());
                }
                net.apply(record);
            }
            match list.next_page_token {
                _ if cut => break,
                Some(next) => page = Some(next),
                None => {
                    let current = progress.latest.or(progress.last_record);
                    let current = current.map_or_else(|| cursor.clone(), SyncCursor);
                    return self.finish(net, current, false).await;
                }
            }
        }
        let (resume, more) = progress.resume(cursor);
        self.finish(net, resume, more).await
    }

    async fn history_page(
        &self,
        cursor: &SyncCursor,
        page: Option<&str>,
    ) -> Result<HistoryList, ProviderError> {
        let mut query = vec![
            ("startHistoryId", cursor.0.clone()),
            ("maxResults", MAX_HISTORY_RECORDS.to_string()),
        ];
        query.extend(
            HISTORY_TYPES
                .iter()
                .map(|t| ("historyTypes", (*t).to_string())),
        );
        if let Some(page) = page {
            query.push(("pageToken", page.to_string()));
        }
        match self.api.get_json("history", &query, COST_HISTORY).await {
            Err(ProviderError::NotFound) => Err(ProviderError::CursorExpired),
            other => other,
        }
    }

    /// Fetch metadata for the added messages and assemble the set.
    async fn finish(
        &self,
        net: Net,
        cursor: SyncCursor,
        more: bool,
    ) -> Result<ChangeSet, ProviderError> {
        let fetched = self.metadata(&net.added).await?;
        let mut relabels = net.relabels;
        let relabeled = net
            .relabel_order
            .iter()
            .filter_map(|id| relabels.remove(id))
            .collect();
        Ok(ChangeSet {
            added: fetched.envelopes,
            deleted: net.deleted,
            relabeled,
            skipped: fetched.skipped,
            cursor,
            more,
        })
    }
}
