//! `threads.modify` and `threads.trash`: one POST per thread. Thread and label ids are checked
//! like every id Rail puts in a path before anything is sent.

use serde_json::json;

use super::{COST_THREAD_MODIFY, COST_THREAD_TRASH, GmailProvider, MAX_LABELS_PER_MODIFY, safe_id};
use crate::provider::{ProviderError, RemoteId};

/// The longest thread or label id Rail puts in a path or a body.
const MAX_ACTION_ID_BYTES: usize = 128;

fn checked(ids: &[RemoteId], what: &str) -> Result<(), ProviderError> {
    if ids.iter().all(|id| safe_id(id, MAX_ACTION_ID_BYTES)) {
        Ok(())
    } else {
        Err(ProviderError::permanent(&format!(
            "a {what} id is not one Rail sends to Gmail"
        )))
    }
}

impl GmailProvider {
    pub(crate) async fn modify(
        &self,
        ids: &[RemoteId],
        add: &[RemoteId],
        remove: &[RemoteId],
    ) -> Result<(), ProviderError> {
        checked(ids, "thread")?;
        checked(add, "label")?;
        checked(remove, "label")?;
        if add.len() > MAX_LABELS_PER_MODIFY || remove.len() > MAX_LABELS_PER_MODIFY {
            return Err(ProviderError::permanent(
                "Gmail takes at most 100 labels per change",
            ));
        }
        let body = json!({"addLabelIds": add, "removeLabelIds": remove});
        for id in ids {
            self.api
                .post_json(
                    &format!("threads/{id}/modify"),
                    Some(&body),
                    COST_THREAD_MODIFY,
                )
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn trash(&self, ids: &[RemoteId]) -> Result<(), ProviderError> {
        checked(ids, "thread")?;
        for id in ids {
            self.api
                .post_json(&format!("threads/{id}/trash"), None, COST_THREAD_TRASH)
                .await?;
        }
        Ok(())
    }
}
