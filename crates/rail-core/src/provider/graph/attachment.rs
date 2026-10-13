//! One attachment's bytes: `GET me/messages/{id}/attachments/{attachmentId}/$value` (a file's
//! bytes, an item's MIME). A reference attachment (a OneDrive or SharePoint link) has none:
//! Graph answers 405, and Rail says where to reach it. Attachment ids are taken as Graph gives
//! them; that they stay the same between reads is checked live.

use super::errors::is_refusal;
use super::{COST_REQUEST, GraphProvider, graph_id};
use crate::provider::ProviderError;

/// A reference attachment's refusal.
pub const REFERENCE_ATTACHMENT: &str = "This attachment is a link (OneDrive or SharePoint); open the message in Outlook on the web to reach it.";

impl GraphProvider {
    pub(crate) async fn attachment_bytes(
        &self,
        message: &str,
        part: &str,
    ) -> Result<Vec<u8>, ProviderError> {
        if !graph_id(message) || !graph_id(part) {
            return Err(ProviderError::permanent(
                "an id is not one Rail sends to Microsoft 365",
            ));
        }
        let path = format!("me/messages/{message}/attachments/{part}/$value");
        let fetched = self
            .api
            .request_bytes(reqwest::Method::GET, &path, None, COST_REQUEST, true)
            .await;
        match fetched {
            Ok(Some(bytes)) => Ok(bytes),
            Ok(None) => Err(ProviderError::permanent(
                "this attachment is larger than Rail reads",
            )),
            Err(error) if is_refusal(&error, 405) => {
                Err(ProviderError::permanent(REFERENCE_ATTACHMENT))
            }
            Err(error) => Err(error),
        }
    }
}
