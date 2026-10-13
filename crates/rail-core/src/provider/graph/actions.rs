//! Thread changes for Microsoft 365. Part A sends only "mark read": `Change::Read { on: true }`
//! becomes `PATCH me/messages/{id}` `{"isRead": true}` on each stored message of each thread,
//! through `$batch` (a throttled item sent again alone); a 404 means the message is gone, which
//! is done. Every other change is not supported yet.

use serde_json::json;

use super::batch::BatchRequest;
use super::{GraphProvider, graph_id, not_yet};
use crate::provider::{Change, ProviderError, RemoteId, ThreadTarget};

impl GraphProvider {
    pub(crate) async fn apply_change(
        &self,
        threads: &[ThreadTarget],
        change: &Change,
    ) -> Result<(), ProviderError> {
        if *change != (Change::Read { on: true }) {
            return Err(not_yet());
        }
        let ids: Vec<&RemoteId> = threads
            .iter()
            .flat_map(|t| t.messages.iter().map(|m| &m.id))
            .collect();
        if !ids.iter().all(|id| graph_id(id)) {
            return Err(ProviderError::permanent(
                "a message id is not one Rail sends to Microsoft 365",
            ));
        }
        let requests: Vec<BatchRequest> = ids
            .iter()
            .map(|id| BatchRequest::patch(format!("/me/messages/{id}"), json!({"isRead": true})))
            .collect();
        for answer in self.batch(&requests).await? {
            answer.into_body()?;
        }
        Ok(())
    }
}
