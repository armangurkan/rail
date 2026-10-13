//! Sending through Graph: the draft is saved (found by its id or its `Message-ID`, else made),
//! its attachments reconciled, then `POST me/messages/{id}/send`, tried once (202: accepted).
//! The sent copy keeps the draft's immutable id and arrives in Sent Items through the delta.
//! "Sent already" is a message with Rail's `Message-ID` that is no longer a draft, or the draft
//! Rail kept the id of no longer being one.

use super::drafts::{DraftHit, refused_id};
use super::{COST_REQUEST, GraphProvider, graph_id};
use crate::provider::{OutgoingMail, ProviderError, SentReceipt};

impl GraphProvider {
    pub(crate) async fn send_mail(
        &self,
        mail: &OutgoingMail,
    ) -> Result<SentReceipt, ProviderError> {
        let ids = self.save_draft(mail).await?;
        let path = format!("me/messages/{}/send", ids.draft_id);
        self.api
            .post_json_bytes(&path, None, COST_REQUEST, false)
            .await?;
        tracing::info!(draft = %ids.draft_id, "a draft was sent");
        Ok(SentReceipt {
            message_id: Some(ids.draft_id),
            thread_id: None,
        })
    }

    pub(crate) async fn was_sent(&self, mail: &OutgoingMail) -> Result<bool, ProviderError> {
        let hits = self.messages_with_id(&mail.outgoing.message_id).await?;
        if hits.iter().any(|hit| !hit.is_draft) {
            return Ok(true);
        }
        match &mail.remote {
            Some(remote) => self.sent_by_draft_id(&remote.draft_id).await,
            None => Ok(false),
        }
    }

    /// Whether the message with the kept draft id is no longer a draft; gone is not sent.
    async fn sent_by_draft_id(&self, id: &str) -> Result<bool, ProviderError> {
        Ok(self
            .message_kind(id)
            .await?
            .is_some_and(|hit| !hit.is_draft))
    }

    /// The message `id` with its `isDraft`; `None` when Outlook has none.
    pub(super) async fn message_kind(&self, id: &str) -> Result<Option<DraftHit>, ProviderError> {
        if !graph_id(id) {
            return Err(refused_id());
        }
        let query = [("$select", "id,isDraft".to_string())];
        let path = format!("me/messages/{id}");
        match self
            .api
            .get_json::<DraftHit>(&path, &query, COST_REQUEST)
            .await
        {
            Ok(hit) => Ok(Some(hit)),
            Err(ProviderError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
    }
}
