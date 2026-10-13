//! A draft's attachments made Rail's, after every save and before every send: the draft's
//! file attachments that are not inline are listed and matched to Rail's by (cleaned name,
//! size), in order, each once. A matched one stays; one only in Outlook is deleted; one only in
//! Rail is uploaded (a carried forward original is fetched from its message first). Inline and
//! item attachments are never touched. Logs carry counts.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use reqwest::Method;
use serde::Deserialize;
use serde_json::json;

use super::drafts::unexpected;
use super::wire::Page;
use super::{COST_REQUEST, GraphProvider, graph_id};
use crate::provider::gmail::mime::sanitize_filename;
use crate::provider::{Carried, OutgoingAttachment, OutgoingMail, ProviderError};

/// Graph's type of a file attachment.
pub(super) const FILE_ATTACHMENT: &str = "#microsoft.graph.fileAttachment";
/// Attachments one listing asks for; Rail sends at most 50.
const MAX_LISTED: usize = 100;
/// Below this a file is posted whole; at it or above it goes through an upload session.
pub const SMALL_ATTACHMENT_BYTES: usize = 3 * 1024 * 1024;

#[derive(Deserialize)]
struct ListedWire {
    id: String,
    name: Option<String>,
    size: Option<i64>,
    #[serde(rename = "isInline", default)]
    is_inline: bool,
    #[serde(rename = "@odata.type")]
    odata_type: Option<String>,
}

/// One file attachment of the Outlook draft: its id, and the cleaned name and size it is
/// matched by.
pub(super) struct Listed {
    pub id: String,
    pub name: String,
    pub size: usize,
}

impl std::fmt::Debug for Listed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listed")
            .field("id", &self.id)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// What a reconcile changes: the listed attachments to delete, and Rail's to upload (indexes
/// into the message's attachments).
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Changes {
    pub delete: Vec<String>,
    pub upload: Vec<usize>,
}

/// Rail's attachments matched to the draft's by (cleaned name, size), in order, each listed one
/// at most once.
pub(super) fn changes(listed: &[Listed], wanted: &[OutgoingAttachment]) -> Changes {
    let mut taken = vec![false; listed.len()];
    let mut upload = Vec::new();
    for (index, attachment) in wanted.iter().enumerate() {
        let name = sanitize_filename(&attachment.name);
        let size = attachment.size();
        let found = listed
            .iter()
            .zip(&taken)
            .position(|(l, done)| !done && l.name == name && l.size == size);
        match found.and_then(|i| taken.get_mut(i)) {
            Some(done) => *done = true,
            None => upload.push(index),
        }
    }
    let delete = listed
        .iter()
        .zip(&taken)
        .filter(|(_, done)| !**done)
        .map(|(l, _)| l.id.clone())
        .collect();
    Changes { delete, upload }
}

fn listed_of(wire: ListedWire) -> Result<Listed, ProviderError> {
    if !graph_id(&wire.id) {
        return Err(unexpected());
    }
    Ok(Listed {
        name: sanitize_filename(wire.name.as_deref().unwrap_or_default()),
        size: usize::try_from(wire.size.unwrap_or(0)).unwrap_or(0),
        id: wire.id,
    })
}

impl GraphProvider {
    /// Make `draft`'s attachments `mail`'s. `draft` passed `graph_id` when it was saved.
    pub(super) async fn reconcile(
        &self,
        draft: &str,
        mail: &OutgoingMail,
    ) -> Result<(), ProviderError> {
        let listed = self.file_attachments(draft).await?;
        let attachments = &mail.outgoing.attachments;
        let changes = changes(&listed, attachments);
        for id in &changes.delete {
            self.delete_attachment(draft, id).await?;
        }
        for attachment in changes.upload.iter().filter_map(|&i| attachments.get(i)) {
            self.upload(draft, attachment).await?;
        }
        tracing::info!(
            draft,
            kept = attachments.len() - changes.upload.len(),
            deleted = changes.delete.len(),
            uploaded = changes.upload.len(),
            "a draft's attachments were reconciled"
        );
        Ok(())
    }

    /// The draft's file attachments that are not inline.
    async fn file_attachments(&self, draft: &str) -> Result<Vec<Listed>, ProviderError> {
        let query = [
            ("$select", "id,name,size,contentType,isInline".to_string()),
            ("$top", MAX_LISTED.to_string()),
        ];
        let path = format!("me/messages/{draft}/attachments");
        let page: Page<ListedWire> = self.api.get_json(&path, &query, COST_REQUEST).await?;
        if page.next_link.is_some() {
            return Err(ProviderError::permanent(
                "the Outlook draft has more attachments than Rail reads",
            ));
        }
        page.value
            .into_iter()
            .filter(|a| a.odata_type.as_deref() == Some(FILE_ATTACHMENT) && !a.is_inline)
            .map(listed_of)
            .collect()
    }

    async fn delete_attachment(&self, draft: &str, id: &str) -> Result<(), ProviderError> {
        let path = format!("me/messages/{draft}/attachments/{id}");
        match self
            .api
            .request_bytes(Method::DELETE, &path, None, COST_REQUEST, true)
            .await
        {
            Ok(_) | Err(ProviderError::NotFound) => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Upload one of Rail's attachments, whole or through a session; a carried original is fetched
    /// from its message first.
    async fn upload(
        &self,
        draft: &str,
        attachment: &OutgoingAttachment,
    ) -> Result<(), ProviderError> {
        let fetched;
        let bytes = match &attachment.carried {
            Some(carried) => {
                fetched = self.carried_bytes(carried).await?;
                fetched.as_slice()
            }
            None => attachment.bytes.as_slice(),
        };
        if bytes.len() < SMALL_ATTACHMENT_BYTES {
            self.upload_small(draft, attachment, bytes).await
        } else {
            self.upload_large(draft, attachment, bytes).await
        }
    }

    async fn carried_bytes(&self, carried: &Carried) -> Result<Vec<u8>, ProviderError> {
        self.attachment_bytes(&carried.message, &carried.part)
            .await
            .map_err(|e| match e {
                ProviderError::NotFound => ProviderError::permanent(
                    "an attachment of this message is no longer in Microsoft 365",
                ),
                other => other,
            })
    }

    /// One file of under 3 MiB, posted whole. Tried once: a retried post could attach it twice.
    async fn upload_small(
        &self,
        draft: &str,
        attachment: &OutgoingAttachment,
        bytes: &[u8],
    ) -> Result<(), ProviderError> {
        let body = json!({
            "@odata.type": FILE_ATTACHMENT,
            "name": sanitize_filename(&attachment.name),
            "contentType": attachment.mime,
            "contentBytes": STANDARD.encode(bytes),
        });
        let path = format!("me/messages/{draft}/attachments");
        self.api
            .post_json_bytes(&path, Some(&body), COST_REQUEST, false)
            .await
            .map(|_| ())
    }
}
