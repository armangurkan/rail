//! Gmail's Drafts: create, update, delete, and the message-to-draft lookup.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{
    COST_DRAFT_CREATE, COST_DRAFT_DELETE, COST_DRAFT_LIST, COST_DRAFT_UPDATE, GmailProvider,
    safe_id,
};
use crate::provider::{DraftIds, PageToken, ProviderError, RemoteId};

/// `drafts.list` pages walked at most when looking a message up.
pub const MAX_DRAFT_LIST_PAGES: usize = 10;
/// Drafts per `drafts.list` page.
pub const DRAFT_LIST_PAGE: u32 = 500;
const MAX_ID_BYTES: usize = 64;

#[derive(Deserialize)]
struct DraftWire {
    id: String,
    message: MessageRef,
}

#[derive(Deserialize)]
struct MessageRef {
    id: String,
}

#[derive(Deserialize, Default)]
struct DraftListWire {
    #[serde(default)]
    drafts: Vec<DraftWire>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}

fn refused(what: &str) -> ProviderError {
    ProviderError::permanent(&format!("{what} is not one Rail sends to Gmail"))
}

fn message_body(raw: &[u8], thread_id: Option<&str>) -> Result<Value, ProviderError> {
    let mut message = json!({ "raw": URL_SAFE_NO_PAD.encode(raw) });
    if let Some(thread) = thread_id {
        if !safe_id(thread, MAX_ID_BYTES) {
            return Err(refused("a thread id"));
        }
        message["threadId"] = json!(thread);
    }
    Ok(message)
}

/// The ids of a saved draft; an answer Rail cannot read is a permanent error that says the
/// draft may exist (never "not saved": a retry would make a second one).
fn ids(answer: Option<&[u8]>) -> Result<DraftIds, ProviderError> {
    let unreadable =
        || ProviderError::permanent("Gmail saved the draft but its answer was unreadable");
    let wire: DraftWire = answer
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .ok_or_else(unreadable)?;
    if !safe_id(&wire.id, MAX_ID_BYTES) || !safe_id(&wire.message.id, MAX_ID_BYTES) {
        return Err(unreadable());
    }
    Ok(DraftIds {
        draft_id: wire.id,
        message_id: wire.message.id,
    })
}

impl GmailProvider {
    pub(crate) async fn create_draft(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<DraftIds, ProviderError> {
        let body = json!({ "message": message_body(raw, thread_id)? });
        let answer = self
            .api
            .request_bytes(
                reqwest::Method::POST,
                "drafts",
                Some(&body),
                COST_DRAFT_CREATE,
                false,
            )
            .await?;
        ids(answer.as_deref())
    }

    pub(crate) async fn update_draft(
        &self,
        draft_id: &str,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<DraftIds, ProviderError> {
        if !safe_id(draft_id, MAX_ID_BYTES) {
            return Err(refused("a draft id"));
        }
        let body = json!({ "id": draft_id, "message": message_body(raw, thread_id)? });
        let path = format!("drafts/{draft_id}");
        let answer = self
            .api
            .request_bytes(
                reqwest::Method::PUT,
                &path,
                Some(&body),
                COST_DRAFT_UPDATE,
                true,
            )
            .await?;
        ids(answer.as_deref())
    }

    pub(crate) async fn delete_draft(&self, draft_id: &str) -> Result<(), ProviderError> {
        if !safe_id(draft_id, MAX_ID_BYTES) {
            return Err(refused("a draft id"));
        }
        let path = format!("drafts/{draft_id}");
        self.api
            .request_bytes(
                reqwest::Method::DELETE,
                &path,
                None,
                COST_DRAFT_DELETE,
                true,
            )
            .await
            .map(|_| ())
    }

    pub(crate) async fn find_draft(
        &self,
        message_id: &str,
    ) -> Result<Option<RemoteId>, ProviderError> {
        if !safe_id(message_id, MAX_ID_BYTES) {
            return Err(refused("a message id"));
        }
        let mut page: Option<PageToken> = None;
        for _ in 0..MAX_DRAFT_LIST_PAGES {
            let mut query = vec![("maxResults", DRAFT_LIST_PAGE.to_string())];
            if let Some(token) = &page {
                query.push(("pageToken", token.0.clone()));
            }
            let list: DraftListWire = self.api.get_json("drafts", &query, COST_DRAFT_LIST).await?;
            if let Some(found) = list
                .drafts
                .into_iter()
                .find(|draft| draft.message.id == message_id)
            {
                // A found draft with an unusable id is an error, never "not there".
                return if safe_id(&found.id, MAX_ID_BYTES) {
                    Ok(Some(found.id))
                } else {
                    Err(ProviderError::permanent(
                        "Gmail's answer did not have the expected shape",
                    ))
                };
            }
            match list.next_page_token.as_deref().and_then(PageToken::checked) {
                Some(next) => page = Some(next),
                None => return Ok(None),
            }
        }
        Ok(None)
    }
}
