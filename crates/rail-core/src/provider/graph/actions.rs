//! Thread changes for Microsoft 365. Part A sends only "mark read": `remove [UNREAD_LABEL]`
//! becomes `PATCH me/messages/{id}` `{"isRead": true}` on each stored message of each thread,
//! through `$batch` (a throttled item sent again alone); a 404 means the message is gone, which
//! is done. Every other change, and trash, is not supported yet.

use serde_json::json;

use super::batch::BatchRequest;
use super::{GraphProvider, graph_id, not_yet};
use crate::provider::{ProviderError, RemoteId, ThreadTarget, UNREAD_LABEL};

/// Whether `add` and `remove` mark read: the one change Part A sends.
fn marks_read(add: &[RemoteId], remove: &[RemoteId]) -> bool {
    add.is_empty() && remove.len() == 1 && remove[0] == UNREAD_LABEL
}

impl GraphProvider {
    pub(crate) async fn modify(
        &self,
        threads: &[ThreadTarget],
        add: &[RemoteId],
        remove: &[RemoteId],
    ) -> Result<(), ProviderError> {
        if !marks_read(add, remove) {
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
