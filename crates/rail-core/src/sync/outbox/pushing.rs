//! The push of a `PushDraft` row (the provider creates the draft, or updates it by its id; one
//! it lost is created again) and the delete of a `DeleteDraft` row (gone already is done). Logs
//! carry row and draft ids only.

use super::Worker;
use super::compose;
use crate::ops::DraftId;
use crate::provider::{DraftIds, ProviderError};
use crate::store::drafts::ListedFile;

impl Worker {
    /// Create or update the draft's copy at the provider; the provider's ids for it, and the
    /// sizes it lists for the draft's files.
    pub(super) async fn push_draft(
        &self,
        draft: DraftId,
    ) -> Result<(DraftIds, Vec<ListedFile>), ProviderError> {
        let loaded = self.load(draft).await?;
        if !loaded.attachments_readable {
            return Err(ProviderError::permanent(
                "the draft's attachments could not be read",
            ));
        }
        let paths = compose::file_paths(&loaded.assembly);
        let mail = compose::attach(
            loaded.assembly,
            self.provider.as_ref(),
            self.kind.provider_name(),
        )
        .await?;
        let saved = self.provider.draft_save(&mail).await?;
        let listed = compose::listed_files(&paths, &mail, &saved.listed);
        Ok((saved.ids, listed))
    }

    /// Delete a provider draft; one gone already is done.
    pub(super) async fn delete_remote_draft(&self, id: &str) -> Result<(), ProviderError> {
        match self.provider.draft_delete(id).await {
            Ok(()) | Err(ProviderError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }
}
