//! The push of a `PushDraft` row (create, or update by `remote_draft_id`; an update of a
//! draft deleted at Gmail creates it again) and the delete of a `DeleteDraft` row (gone
//! already is done). Logs carry row and draft ids only.

use super::Worker;
use super::compose;
use crate::ops::DraftId;
use crate::provider::gmail::build_message;
use crate::provider::{DraftIds, ProviderError};

impl Worker {
    /// Create or update the draft's Gmail copy; Gmail's ids for it.
    pub(super) async fn push_draft(&self, draft: DraftId) -> Result<DraftIds, ProviderError> {
        let loaded = self.load(draft).await?;
        if !loaded.attachments_readable {
            return Err(ProviderError::permanent(
                "the draft's attachments could not be read",
            ));
        }
        let remote = loaded.remote_draft_id.clone();
        let thread = loaded.remote_thread.clone();
        let outgoing = compose::attach(loaded.assembly, self.provider.as_ref()).await?;
        let raw = build_message(&outgoing)?;
        let Some(id) = remote else {
            return self.create_draft(draft, &raw, thread.as_deref()).await;
        };
        match self
            .provider
            .draft_update(&id, &raw, thread.as_deref())
            .await
        {
            Err(ProviderError::NotFound) => {
                tracing::info!(account = %self.name, draft, "the Gmail draft is gone; saving it again");
                self.create_draft(draft, &raw, thread.as_deref()).await
            }
            other => other,
        }
    }

    /// A create; one in a thread Gmail no longer has is saved on its own (a 404 is a refusal
    /// before anything is saved, so this cannot make two).
    async fn create_draft(
        &self,
        draft: DraftId,
        raw: &[u8],
        thread: Option<&str>,
    ) -> Result<DraftIds, ProviderError> {
        match self.provider.draft_create(raw, thread).await {
            Err(ProviderError::NotFound) if thread.is_some() => {
                tracing::info!(account = %self.name, draft, "the draft's thread is gone at Gmail; saving it on its own");
                self.provider.draft_create(raw, None).await
            }
            other => other,
        }
    }

    /// Delete a Gmail draft; one gone already is done.
    pub(super) async fn delete_remote_draft(&self, id: &str) -> Result<(), ProviderError> {
        match self.provider.draft_delete(id).await {
            Ok(()) | Err(ProviderError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}
