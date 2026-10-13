//! Microsoft 365 drafts through Graph's message model. A new message is `POST me/messages`; a
//! reply, reply-all or forward is `createReply`, `createReplyAll` or `createForward` on its
//! source, then a `PATCH` of Rail's fields (subject, the text body, recipients, and Rail's
//! `Message-ID` as `internetMessageId`). An update is one `PATCH`; a draft deleted in Outlook
//! (404) is created again, and a source Outlook no longer has makes a new message. A save with
//! no held id looks for a draft carrying Rail's `Message-ID` first, so a draft made before a
//! crash is updated rather than made twice. Creates are tried once. Logs carry ids only.

use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::wire::Page;
use super::{COST_REQUEST, GraphProvider, graph_id};
use crate::ops::MAX_ATTACHMENTS;
use crate::provider::gmail::mime::{
    MAX_MESSAGE_BYTES, estimated_raw_size, too_large_text, validate,
};
use crate::provider::{DraftIds, Mailbox, OutgoingMail, ProviderError, RemoteId};
use crate::store::drafts::DraftKind;

/// A message as a lookup by id or by `internetMessageId` reads it. A missing `isDraft` reads
/// as sent: the safe side for "was it sent already".
#[derive(Deserialize)]
pub(super) struct DraftHit {
    pub id: String,
    #[serde(rename = "isDraft", default)]
    pub is_draft: bool,
}

pub(super) fn refused_id() -> ProviderError {
    ProviderError::permanent("an id is not one Rail sends to Microsoft 365")
}

pub(super) fn unexpected() -> ProviderError {
    ProviderError::permanent("Microsoft 365's answer did not have the expected shape")
}

fn recipients(list: &[Mailbox]) -> Value {
    Value::Array(
        list.iter()
            .map(|m| {
                let mut address = json!({"address": m.address});
                if let Some(name) = m.name.as_deref().filter(|n| !n.trim().is_empty()) {
                    address["name"] = json!(name);
                }
                json!({"emailAddress": address})
            })
            .collect(),
    )
}

/// The draft's fields as Graph's message JSON: what a create carries and a `PATCH` sets.
/// The text goes as plain text, Rail's quote and signature already in it.
pub(super) fn draft_json(mail: &OutgoingMail) -> Value {
    let o = &mail.outgoing;
    json!({
        "subject": o.subject,
        "body": {"contentType": "text", "content": o.body},
        "toRecipients": recipients(&o.to),
        "ccRecipients": recipients(&o.cc),
        "bccRecipients": recipients(&o.bcc),
        "internetMessageId": o.message_id,
    })
}

/// The `$filter` that finds messages by their `Message-ID`: in single quotes, a quote doubled.
pub(super) fn message_id_filter(id: &str) -> String {
    format!("internetMessageId eq '{}'", id.replace('\'', "''"))
}

/// Refuse what Rail would not send: an address or id the MIME builder refuses, more than
/// `MAX_ATTACHMENTS`, and a message over 25 MiB as the builder counts it. Sizes and counts
/// only in the error.
fn checked(mail: &OutgoingMail) -> Result<(), ProviderError> {
    validate(&mail.outgoing)?;
    let sizes: Vec<usize> = mail.outgoing.attachments.iter().map(|a| a.size()).collect();
    if sizes.len() > MAX_ATTACHMENTS {
        return Err(ProviderError::permanent(&format!(
            "at most {MAX_ATTACHMENTS} attachments"
        )));
    }
    let estimated = estimated_raw_size(mail.outgoing.body.len(), &sizes);
    if estimated > MAX_MESSAGE_BYTES {
        return Err(ProviderError::permanent(&too_large_text(estimated)));
    }
    Ok(())
}

/// The id of a draft Graph made; an answer Rail cannot read is a permanent error that says the
/// draft may exist (the next save finds it by its `Message-ID`).
fn created_id(answer: Option<&[u8]>) -> Result<RemoteId, ProviderError> {
    let unreadable =
        || ProviderError::permanent("Microsoft 365 saved the draft but its answer was unreadable");
    let hit: DraftHit = answer
        .and_then(|bytes| serde_json::from_slice(bytes).ok())
        .ok_or_else(unreadable)?;
    if graph_id(&hit.id) {
        Ok(hit.id)
    } else {
        Err(unreadable())
    }
}

/// The action that makes `kind` on its source; `None` for a new message.
fn create_action(kind: DraftKind) -> Option<&'static str> {
    match kind {
        DraftKind::New => None,
        DraftKind::Reply => Some("createReply"),
        DraftKind::ReplyAll => Some("createReplyAll"),
        DraftKind::Forward => Some("createForward"),
    }
}

impl GraphProvider {
    /// Save `mail` as an Outlook draft: update the one Rail holds or finds by its
    /// `Message-ID`, else create it. Its ids (both Graph's id).
    pub(crate) async fn save_draft(&self, mail: &OutgoingMail) -> Result<DraftIds, ProviderError> {
        checked(mail)?;
        let known = match &mail.remote {
            Some(remote) => Some(remote.draft_id.clone()),
            None => self.draft_by_message_id(&mail.outgoing.message_id).await?,
        };
        let id = match known {
            Some(id) => self.update_or_create(&id, mail).await?,
            None => self.create(mail).await?,
        };
        self.reconcile(&id, mail).await?;
        Ok(DraftIds {
            draft_id: id.clone(),
            message_id: id,
        })
    }

    async fn update_or_create(
        &self,
        id: &str,
        mail: &OutgoingMail,
    ) -> Result<RemoteId, ProviderError> {
        match self.patch_draft(id, mail).await {
            Ok(()) => Ok(id.to_string()),
            Err(ProviderError::NotFound) => {
                tracing::info!(draft = id, "the Outlook draft is gone; saving it again");
                self.create(mail).await
            }
            Err(error) => Err(error),
        }
    }

    async fn patch_draft(&self, id: &str, mail: &OutgoingMail) -> Result<(), ProviderError> {
        if !graph_id(id) {
            return Err(refused_id());
        }
        let path = format!("me/messages/{id}");
        self.api
            .request_bytes(
                Method::PATCH,
                &path,
                Some(&draft_json(mail)),
                COST_REQUEST,
                true,
            )
            .await
            .map(|_| ())
    }

    /// A new draft: on its source for a reply or forward (then patched with Rail's fields), or
    /// a new message when there is no source or Outlook no longer has it.
    async fn create(&self, mail: &OutgoingMail) -> Result<RemoteId, ProviderError> {
        let (Some(action), Some(source)) = (create_action(mail.kind), mail.source.as_deref())
        else {
            return self.create_new(mail).await;
        };
        match self.create_from(source, action).await {
            Ok(id) => match self.patch_draft(&id, mail).await {
                Ok(()) => Ok(id),
                Err(error) => {
                    self.discard_orphan(&id).await;
                    Err(error)
                }
            },
            Err(ProviderError::NotFound) => {
                tracing::info!("the message answered is gone from Outlook; writing a new one");
                self.create_new(mail).await
            }
            Err(error) => Err(error),
        }
    }

    /// Delete a draft just created whose `PATCH` failed: without Rail's `Message-ID` on it the
    /// lookup could not find it, and the next save would make a second one. Best effort; the
    /// outcome is logged by id and code only. (A create whose answer is lost stays a known gap.)
    async fn discard_orphan(&self, id: &str) {
        let path = format!("me/messages/{id}");
        match self
            .api
            .request_bytes(Method::DELETE, &path, None, COST_REQUEST, true)
            .await
        {
            Ok(_) => tracing::info!(draft = id, "removed a draft whose fields could not be set"),
            Err(error) => {
                tracing::warn!(draft = id, code = error.code(), "could not remove it")
            }
        }
    }

    async fn create_new(&self, mail: &OutgoingMail) -> Result<RemoteId, ProviderError> {
        let body = draft_json(mail);
        let answer = self
            .api
            .post_json_bytes("me/messages", Some(&body), COST_REQUEST, false)
            .await?;
        created_id(answer.as_deref())
    }

    async fn create_from(&self, source: &str, action: &str) -> Result<RemoteId, ProviderError> {
        if !graph_id(source) {
            return Err(refused_id());
        }
        let path = format!("me/messages/{source}/{action}");
        let answer = self
            .api
            .post_json_bytes(&path, Some(&json!({})), COST_REQUEST, false)
            .await?;
        created_id(answer.as_deref())
    }

    /// The Outlook draft carrying `message_id`, if one exists.
    async fn draft_by_message_id(
        &self,
        message_id: &str,
    ) -> Result<Option<RemoteId>, ProviderError> {
        let hits = self.messages_with_id(message_id).await?;
        Ok(hits.into_iter().find(|hit| hit.is_draft).map(|hit| hit.id))
    }

    /// Every message whose `internetMessageId` is `message_id`, across folders. An id Rail
    /// would not put in a path is an error, never "not there".
    pub(super) async fn messages_with_id(
        &self,
        message_id: &str,
    ) -> Result<Vec<DraftHit>, ProviderError> {
        let query = [
            ("$filter", message_id_filter(message_id)),
            ("$select", "id,isDraft".to_string()),
        ];
        let page: Page<DraftHit> = self
            .api
            .get_json("me/messages", &query, COST_REQUEST)
            .await?;
        if page.value.iter().any(|hit| !graph_id(&hit.id)) {
            return Err(unexpected());
        }
        Ok(page.value)
    }
}
