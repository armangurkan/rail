//! `changes` and `profile`: the combined cursor's folders walked in order, each folder's delta
//! followed for at most `GRAPH_PAGES_PER_CALL` pages a call across folders. A move between
//! folders is a removal in one and an add in the other, in this call or another: an add wins
//! over a removal of the same id, and a removal left unpaired is checked (`envelopes_of`)
//! before it is reported deleted, so a message Graph still has, in a folder Rail syncs, is
//! upserted where it is now. Bodies never come through the delta. A folder whose link stays over
//! the cap is given up on after three start-overs in a row (`stuck`); a folder of a skipped
//! subtree is set aside for the call (`GraphCursor::set_aside`).

use std::collections::{BTreeMap, HashSet};
use std::sync::PoisonError;

use serde_json::Value;

use super::cursor::{GraphCursor, State};
use super::dates::iso;
use super::errors::on_delta;
use super::message::{Item, item};
use super::search::MESSAGE_FIELDS;
use super::stuck::name_of;
use super::wire::Page;
use super::{COST_REQUEST, GraphProvider};
use crate::provider::{
    ChangeSet, Envelope, EnvelopePage, Profile, ProviderError, Relabel, RemoteId, SyncCursor,
};

/// Delta pages one call reads at most, across folders.
pub const GRAPH_PAGES_PER_CALL: usize = 10;
/// The page-size preference every delta request carries.
pub const PAGE_SIZE: &str = "odata.maxpagesize=100";

/// What the pages of one call add up to.
#[derive(Default)]
struct Net {
    added: BTreeMap<RemoteId, Envelope>,
    /// Removed ids no add has paired, with the folder that removed them.
    removed: BTreeMap<RemoteId, RemoteId>,
    relabels: Vec<Relabel>,
    skipped: Vec<RemoteId>,
}

impl Net {
    fn page(&mut self, folder: &str, items: Vec<Value>) {
        for value in items {
            match item(value, folder) {
                Item::Removed(id) => self.remove(id, folder),
                Item::Full(envelope) => {
                    let id = envelope.remote_id.clone();
                    self.removed.remove(&id);
                    self.relabels.retain(|r| r.remote_id != id);
                    self.added.insert(id, *envelope);
                }
                Item::Partial(relabel) => self.relabel(relabel),
                Item::Unreadable(id) => self.skipped.extend(id),
            }
        }
    }

    /// A flags-only update: applied to the envelope an earlier page added (it is newer), else
    /// kept as a relabel.
    fn relabel(&mut self, relabel: Relabel) {
        if relabel.read.is_none() && relabel.starred.is_none() {
            return;
        }
        let Some(envelope) = self.added.get_mut(&relabel.remote_id) else {
            self.relabels.push(relabel);
            return;
        };
        if let Some(read) = relabel.read {
            envelope.is_read = read;
        }
        if let Some(starred) = relabel.starred {
            envelope.is_starred = starred;
        }
    }

    /// A removal from `folder`. Of a message an earlier page added in another folder, it is
    /// the old folder's side of a move and changes nothing.
    fn remove(&mut self, id: RemoteId, folder: &str) {
        if let Some(added) = self.added.get(&id) {
            if added.folders.first().map(String::as_str) != Some(folder) {
                return;
            }
            self.added.remove(&id);
        }
        self.removed.insert(id, folder.to_string());
    }
}

impl GraphProvider {
    /// A cursor with every folder Rail syncs, none started; Graph cannot count the mailbox.
    pub(crate) async fn read_profile(&self) -> Result<Profile, ProviderError> {
        let folders = self.known_folders().await?;
        let cursor = GraphCursor::start(&folders, self.cutoff_ms()).encode()?;
        Ok(Profile {
            cursor,
            messages_total: None,
        })
    }

    pub(crate) async fn delta(&self, raw: &SyncCursor) -> Result<ChangeSet, ProviderError> {
        let mut cursor = GraphCursor::decode(raw)?;
        let folders = self.known_folders().await?;
        self.stuck().keep_only(&folders);
        cursor.follow(&folders)?;
        cursor.begin_round();
        let mut net = Net::default();
        let mut pages = 0;
        let skipped = self
            .skipped
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        'folders: for id in cursor.order(&folders) {
            if skipped.contains(&id) {
                cursor.set_aside(&id);
                continue;
            }
            if self.stuck().given_up(&id) {
                cursor.park(&id);
                continue;
            }
            while let Some(folder) = cursor
                .folders
                .get(&id)
                .filter(|f| f.state != State::CaughtUp)
            {
                if pages == GRAPH_PAGES_PER_CALL {
                    break 'folders;
                }
                pages += 1;
                let link = folder.link.clone();
                let page = self
                    .delta_page(&id, link, cursor.after)
                    .await
                    .map_err(on_delta)?;
                net.page(&id, page.value);
                let step = cursor.advance(&id, page.next_link, page.delta_link)?;
                if self.stuck().count(&id, step, &name_of(&folders, &id)) {
                    cursor.park(&id);
                }
            }
        }
        self.finish(net, cursor).await
    }

    /// One page of `folder`'s delta: its link, or (not started) the initial request.
    async fn delta_page(
        &self,
        folder: &str,
        link: Option<String>,
        after: Option<i64>,
    ) -> Result<Page<Value>, ProviderError> {
        if let Some(link) = link {
            return self
                .api
                .get_json_with(&link, &[], COST_REQUEST, &[PAGE_SIZE])
                .await;
        }
        let mut query = vec![("$select", MESSAGE_FIELDS.to_string())];
        if let Some(after) = after {
            query.push(("$filter", format!("receivedDateTime ge {}", iso(after))));
        }
        let path = format!("me/mailFolders/{folder}/messages/delta");
        self.api
            .get_json_with(&path, &query, COST_REQUEST, &[PAGE_SIZE])
            .await
    }

    /// The unpaired removals as Graph has them now. One it refuses for good is skipped, so
    /// kept and not deleted: one refused message must not stall the account every pass.
    async fn removals_checked(&self, ids: &[RemoteId]) -> Result<EnvelopePage, ProviderError> {
        let (mut page, refused) = self.envelopes_answered(ids).await?;
        if !refused.is_empty() {
            tracing::warn!(
                count = refused.len(),
                class = "permanent",
                "Microsoft 365 refused checking removed messages; keeping them"
            );
            page.skipped.extend(refused.into_iter().map(|(id, _)| id));
        }
        Ok(page)
    }

    /// Check the unpaired removals, and assemble the set.
    async fn finish(&self, net: Net, cursor: GraphCursor) -> Result<ChangeSet, ProviderError> {
        let removed: Vec<RemoteId> = net.removed.into_keys().collect();
        let checked = if removed.is_empty() {
            EnvelopePage::default()
        } else {
            self.removals_checked(&removed).await?
        };
        let mut added: Vec<Envelope> = net.added.into_values().collect();
        let mut kept: HashSet<RemoteId> = checked.skipped.iter().cloned().collect();
        for envelope in checked.envelopes {
            let synced = envelope
                .folders
                .first()
                .is_some_and(|f| cursor.folders.contains_key(f));
            if synced {
                kept.insert(envelope.remote_id.clone());
                added.push(envelope);
            }
        }
        let fresh: HashSet<RemoteId> = added.iter().map(|e| e.remote_id.clone()).collect();
        let relabeled = net
            .relabels
            .into_iter()
            .filter(|r| !fresh.contains(&r.remote_id))
            .collect();
        Ok(ChangeSet {
            added,
            deleted: removed
                .into_iter()
                .filter(|id| !kept.contains(id))
                .collect(),
            relabeled,
            skipped: net.skipped.into_iter().chain(checked.skipped).collect(),
            more: cursor.more(),
            cursor: cursor.encode()?,
        })
    }
}
