//! Microsoft 365 drafts through Graph's message model. A new message is `POST me/messages`; a
//! reply, reply-all or forward is `createReply`, `createReplyAll` or `createForward` on its
//! source, then a `PATCH` of Rail's fields (subject, the text body, recipients, and Rail's
//! `Message-ID` as `internetMessageId`). An update is one `PATCH`; a draft deleted in Outlook
//! is created again, and a source Outlook no longer has makes a new message. Deleted means gone
//! (404) or outside the Drafts folder (Outlook moves a deleted draft to Deleted Items, where it
//! keeps its id and `isDraft`); the copy there is left alone. A save with no held id looks for a
//! draft in Drafts carrying Rail's `Message-ID` first, so a draft made before a crash is updated
//! rather than made twice. Creates are tried once. Logs carry ids only.

use reqwest::Method;
use serde::Deserialize;
use serde_json::{Value, json};

use super::wire::{IdOnly, Page};
use super::{COST_REQUEST, GraphProvider, graph_id};
use crate::ops::MAX_ATTACHMENTS;
use crate::provider::gmail::mime::{
    MAX_MESSAGE_BYTES, estimated_raw_size, too_large_text, validate,
};
use crate::provider::{DraftIds, FolderKind, Mailbox, OutgoingMail, ProviderError, RemoteId};
use crate::store::drafts::DraftKind;

/// A message as a lookup by id or by `internetMessageId` (or a `PATCH`'s answer) reads it. A
/// missing `isDraft` reads as sent: the safe side for "was it sent already". `parentFolderId`
/// is there only where it was asked for.
#[derive(Deserialize)]
pub(super) struct DraftHit {
    pub id: String,
    #[serde(rename = "isDraft", default)]
    pub is_draft: bool,
    #[serde(rename = "parentFolderId", default)]
    pub parent_folder_id: Option<String>,
}

/// Whether `hit` is in the Drafts folder `drafts`; when either folder is unknown it counts as
/// in, as before Rail read folders here.
fn in_drafts(hit: &DraftHit, drafts: Option<&str>) -> bool {
    match (hit.parent_folder_id.as_deref(), drafts) {
        (Some(folder), Some(drafts)) => folder == drafts,
        _ => true,
    }
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
        return Err(ProviderError::permanent(&too_large_text(
            estimated,
            "Microsoft 365",
        )));
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

    /// `PATCH` the draft `id`; made again when it is gone or, by the `PATCH`'s answer, no
    /// longer in Drafts (the copy in Deleted Items stays there).
    async fn update_or_create(
        &self,
        id: &str,
        mail: &OutgoingMail,
    ) -> Result<RemoteId, ProviderError> {
        let answer = match self.patch_draft(id, mail).await {
            Ok(answer) => answer,
            Err(ProviderError::NotFound) => {
                tracing::info!(draft = id, "the Outlook draft is gone; saving it again");
                return self.create(mail).await;
            }
            Err(error) => return Err(error),
        };
        let hit = answer.and_then(|bytes| serde_json::from_slice::<DraftHit>(&bytes).ok());
        let Some(hit) = hit.filter(|h| h.parent_folder_id.is_some()) else {
            return Ok(id.to_string());
        };
        if in_drafts(&hit, self.drafts_folder().await?.as_deref()) {
            return Ok(id.to_string());
        }
        tracing::info!(draft = id, "the Outlook draft left Drafts; saving it again");
        self.create(mail).await
    }

    /// `PATCH` Rail's fields onto the draft `id`; Graph's answer (the message), unparsed.
    async fn patch_draft(
        &self,
        id: &str,
        mail: &OutgoingMail,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
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
    }

    /// The Drafts folder's id: from the last folder listing, else `GET me/mailFolders/drafts`;
    /// `None` when the mailbox has none.
    async fn drafts_folder(&self) -> Result<Option<RemoteId>, ProviderError> {
        if let Some(id) = self.known_folder(FolderKind::Drafts) {
            return Ok(Some(id));
        }
        let query = [("$select", "id".to_string())];
        match self
            .api
            .get_json::<IdOnly>("me/mailFolders/drafts", &query, COST_REQUEST)
            .await
        {
            Ok(found) if graph_id(&found.id) => Ok(Some(found.id)),
            Ok(_) | Err(ProviderError::NotFound) => Ok(None),
            Err(error) => Err(error),
        }
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
                Ok(_) => Ok(id),
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

    /// The Outlook draft in Drafts carrying `message_id`, if one exists. A draft moved to
    /// Deleted Items shares Rail's `Message-ID` with one made again since, and is skipped.
    async fn draft_by_message_id(
        &self,
        message_id: &str,
    ) -> Result<Option<RemoteId>, ProviderError> {
        let drafts: Vec<DraftHit> = self
            .messages_with_id(message_id)
            .await?
            .into_iter()
            .filter(|hit| hit.is_draft)
            .collect();
        let folder = if drafts.iter().any(|hit| hit.parent_folder_id.is_some()) {
            self.drafts_folder().await?
        } else {
            None
        };
        Ok(drafts
            .into_iter()
            .find(|hit| in_drafts(hit, folder.as_deref()))
            .map(|hit| hit.id))
    }

    /// Every message whose `internetMessageId` is `message_id`, across folders, with its
    /// folder. An id Rail would not put in a path is an error, never "not there".
    pub(super) async fn messages_with_id(
        &self,
        message_id: &str,
    ) -> Result<Vec<DraftHit>, ProviderError> {
        let query = [
            ("$filter", message_id_filter(message_id)),
            ("$select", "id,isDraft,parentFolderId".to_string()),
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

    /// Delete the draft (Outlook moves it to Deleted Items). Only a message that is still a
    /// draft is deleted: one already sent (a row that kept the sent message's immutable id) or
    /// gone is done, so a discard never moves sent mail away.
    pub(crate) async fn delete_draft(&self, id: &str) -> Result<(), ProviderError> {
        if !self.message_kind(id).await?.is_some_and(|hit| hit.is_draft) {
            return Ok(());
        }
        let path = format!("me/messages/{id}");
        match self
            .api
            .request_bytes(Method::DELETE, &path, None, COST_REQUEST, true)
            .await
        {
            Ok(_) | Err(ProviderError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// The draft that is message `id`: on Graph a Drafts-folder message's id is its draft's.
    pub(crate) async fn find_draft(&self, id: &str) -> Result<Option<RemoteId>, ProviderError> {
        Ok(self
            .message_kind(id)
            .await?
            .filter(|hit| hit.is_draft && hit.id == id)
            .map(|hit| hit.id))
    }
}
