//! The bytes of one attachment. Rail keeps a part's MIME part id; Gmail's `attachments.get`
//! wants the part's opaque `body.attachmentId`, which Gmail does not keep stable across reads.
//! So each fetch reads the message again and looks the part up by its part id.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

use super::body::MAX_PART_DEPTH;
use super::wire::{Message, Part};
use super::{COST_GET, GmailProvider, safe_id};
use crate::provider::ProviderError;

const MAX_MESSAGE_ID_BYTES: usize = 64;

fn gone() -> ProviderError {
    ProviderError::permanent("this attachment is no longer in the message")
}

impl GmailProvider {
    pub(crate) async fn attachment_bytes(
        &self,
        message: &str,
        part: &str,
    ) -> Result<Vec<u8>, ProviderError> {
        if !safe_id(message, MAX_MESSAGE_ID_BYTES) {
            return Err(ProviderError::permanent(
                "a message id is not one Rail sends to Gmail",
            ));
        }
        let query = [("format", "full".to_string())];
        let answer: Message = self
            .api
            .get_json(&format!("messages/{message}"), &query, COST_GET)
            .await?;
        if answer.id.as_deref() != Some(message) {
            return Err(gone());
        }
        let payload = answer.payload.ok_or_else(gone)?;
        let found = find_part(&payload, part).ok_or_else(gone)?;
        let data = match (&found.body.data, &found.body.attachment_id) {
            (Some(data), _) => data.clone(),
            (None, Some(attachment)) => self.attachment_data(message, attachment).await?,
            (None, None) => return Err(gone()),
        };
        URL_SAFE_NO_PAD
            .decode(data.trim_end_matches('='))
            .map_err(|_| ProviderError::permanent("Rail could not read this attachment"))
    }
}

/// The part with MIME part id `part_id`, searched at most `MAX_PART_DEPTH` levels down.
pub(crate) fn find_part<'a>(payload: &'a Part, part_id: &str) -> Option<&'a Part> {
    fn go<'a>(part: &'a Part, part_id: &str, depth: usize) -> Option<&'a Part> {
        if depth > MAX_PART_DEPTH {
            return None;
        }
        if part.part_id == part_id {
            return Some(part);
        }
        part.parts
            .iter()
            .find_map(|child| go(child, part_id, depth + 1))
    }
    go(payload, part_id, 0)
}
