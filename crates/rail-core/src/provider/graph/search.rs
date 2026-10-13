//! Server search and envelopes by id. `$search` is KQL over the whole mailbox (`from:`,
//! `subject:`, `to:`, `body:` and plain words; Gmail's operators mean nothing to it), ids only,
//! at most `MAX_GRAPH_SEARCH`. Graph documents neither paging nor `$filter` with `$search`, so
//! Rail asks for one page. Envelopes come through `$batch`.

use super::batch::BatchRequest;
use super::message::envelope;
use super::wire::{IdOnly, MessageWire, Page};
use super::{COST_REQUEST, GraphProvider, graph_id};
use crate::provider::{Envelope, EnvelopePage, ProviderError, RemoteId};

/// The most hits one search asks for: Graph's cap.
pub const MAX_GRAPH_SEARCH: usize = 1_000;
/// The fields an envelope is made from; the delta asks for the same.
pub const MESSAGE_FIELDS: &str = "id,conversationId,internetMessageId,subject,from,sender,toRecipients,ccRecipients,bccRecipients,replyTo,receivedDateTime,sentDateTime,isRead,isDraft,flag,bodyPreview,parentFolderId,hasAttachments";

/// `query` as the KQL phrase `$search` takes: in double quotes, `\` and `"` escaped.
pub(crate) fn kql(query: &str) -> String {
    let escaped = query.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

impl GraphProvider {
    pub(crate) async fn search_ids(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RemoteId>, ProviderError> {
        let limit = limit.clamp(1, MAX_GRAPH_SEARCH);
        let query = [
            ("$search", kql(query)),
            ("$select", "id".to_string()),
            ("$top", limit.to_string()),
        ];
        let page: Page<IdOnly> = self
            .api
            .get_json("me/messages", &query, COST_REQUEST)
            .await?;
        Ok(page
            .value
            .into_iter()
            .map(|m| m.id)
            .filter(|id| graph_id(id))
            .take(limit)
            .collect())
    }

    /// The envelopes of `ids`: gone ones left out, unreadable ones skipped by id.
    pub(crate) async fn envelopes_of(
        &self,
        ids: &[RemoteId],
    ) -> Result<EnvelopePage, ProviderError> {
        let (wanted, refused): (Vec<&RemoteId>, Vec<&RemoteId>) =
            ids.iter().partition(|id| graph_id(id));
        let requests: Vec<BatchRequest> = wanted
            .iter()
            .map(|id| BatchRequest::get(format!("/me/messages/{id}?$select={MESSAGE_FIELDS}")))
            .collect();
        let answers = self.batch(&requests).await?;
        let mut page = EnvelopePage {
            skipped: refused.into_iter().cloned().collect(),
            ..EnvelopePage::default()
        };
        for (id, answer) in wanted.into_iter().zip(answers) {
            let Some(body) = answer.into_body()? else {
                continue;
            };
            let wire = serde_json::from_value::<MessageWire>(body).ok();
            match wire.and_then(|wire| self.located(wire)) {
                Some(found) if found.remote_id == *id => page.envelopes.push(found),
                _ => page.skipped.push(id.clone()),
            }
        }
        Ok(page)
    }

    /// `wire`'s envelope, in the folder it names.
    fn located(&self, wire: MessageWire) -> Option<Envelope> {
        let folder = wire.parent_folder_id.clone().filter(|f| graph_id(f));
        envelope(wire, folder.as_deref())
    }
}
