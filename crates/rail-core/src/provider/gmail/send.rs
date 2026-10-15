//! Sending and the "was it sent already" lookup, restricted to messages labeled SENT.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use serde_json::json;

use super::{COST_LIST, COST_MESSAGE_SEND, GmailProvider, safe_id};
use crate::provider::{ProviderError, RemoteId, SentReceipt};

const MAX_THREAD_ID_BYTES: usize = 64;
const MAX_HEADER_ID_BYTES: usize = 998;

#[derive(Deserialize)]
struct SentWire {
    id: String,
    #[serde(rename = "threadId")]
    thread_id: String,
}

#[derive(Deserialize, Default)]
struct ListWire {
    #[serde(default)]
    messages: Vec<ListedWire>,
}

#[derive(Deserialize)]
struct ListedWire {
    id: String,
}

/// An RFC 5322 message id Rail will put in a Gmail search: `<local@domain>` with nothing a
/// query could misread.
fn safe_header_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_HEADER_ID_BYTES
        && id.starts_with('<')
        && id.ends_with('>')
        && id
            .bytes()
            .all(|b| b.is_ascii_graphic() && !b"\"'\\ ()".contains(&b))
}

/// The ids of an accepted send, or `None`s (logged with the reason, never the body) when the
/// answer was too large, not the expected JSON, or carried ids Rail does not use.
fn receipt(answer: Option<&[u8]>) -> SentReceipt {
    let unknown = SentReceipt {
        message_id: None,
        thread_id: None,
    };
    let Some(bytes) = answer else {
        tracing::warn!(
            reason = "too large",
            "Gmail accepted a send with an unreadable answer"
        );
        return unknown;
    };
    match serde_json::from_slice::<SentWire>(bytes) {
        Ok(sent)
            if safe_id(&sent.id, MAX_THREAD_ID_BYTES)
                && safe_id(&sent.thread_id, MAX_THREAD_ID_BYTES) =>
        {
            SentReceipt {
                message_id: Some(sent.id),
                thread_id: Some(sent.thread_id),
            }
        }
        Ok(_) => {
            tracing::warn!(
                reason = "unsafe ids",
                "Gmail accepted a send with an unreadable answer"
            );
            unknown
        }
        Err(_) => {
            tracing::warn!(
                reason = "malformed",
                "Gmail accepted a send with an unreadable answer"
            );
            unknown
        }
    }
}

impl GmailProvider {
    pub(crate) async fn send_raw(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SentReceipt, ProviderError> {
        if let Some(thread) = thread_id
            && !safe_id(thread, MAX_THREAD_ID_BYTES)
        {
            return Err(ProviderError::permanent(
                "a thread id is not one Rail sends to Gmail",
            ));
        }
        let mut body = json!({ "raw": URL_SAFE_NO_PAD.encode(raw) });
        if let Some(thread) = thread_id {
            body["threadId"] = json!(thread);
        }
        let answer = self
            .api
            .post_json_bytes("messages/send", Some(&body), COST_MESSAGE_SEND, false)
            .await?;
        // A 2xx is a send, whatever its body: an unreadable answer must not read as a failure,
        // or the retry it invites would send the message again.
        Ok(receipt(answer.as_deref()))
    }

    pub(crate) async fn find_by_header_id(
        &self,
        id: &str,
    ) -> Result<Option<RemoteId>, ProviderError> {
        if !safe_header_id(id) {
            return Err(ProviderError::permanent(
                "a message id is not one Rail searches for",
            ));
        }
        let query = [
            ("q", format!("rfc822msgid:{id}")),
            ("labelIds", "SENT".to_string()),
            ("maxResults", "1".to_string()),
        ];
        let list: ListWire = self.api.get_json("messages", &query, COST_LIST).await?;
        // A found message with an unusable id is an error, never "not there": the caller
        // would send the message a second time.
        match list.messages.into_iter().next() {
            None => Ok(None),
            Some(found) if safe_id(&found.id, MAX_THREAD_ID_BYTES) => Ok(Some(found.id)),
            Some(_) => Err(ProviderError::permanent(
                "Gmail's answer did not have the expected shape",
            )),
        }
    }
}
