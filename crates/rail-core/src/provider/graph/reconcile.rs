//! A draft's attachments made Rail's, after every save and before every send: the draft's
//! file attachments that are not inline are listed and matched to Rail's by cleaned name and
//! size (the size Outlook listed for one Rail uploaded or imported, else its bytes), in order,
//! each once. A matched one stays; one only in Outlook is deleted; one only in Rail is uploaded
//! (a carried forward original is fetched from its message first), and the size Outlook lists
//! for it is read back: from the post's answer, else from one listing after the uploads.
//! Inline, item and reference attachments are never touched (`plain_file`). Logs carry counts.

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
/// Whether a listed attachment is a plain file: a `fileAttachment` (with or without the `#` the
/// listing sample leaves out) that is not inline. Only these are matched, fetched and posted;
/// inline pictures, item attachments and reference attachments stay as Outlook has them.
pub(super) fn plain_file(odata_type: Option<&str>, is_inline: bool) -> bool {
    let kind = odata_type.map(|t| t.strip_prefix('#').unwrap_or(t));
    !is_inline && kind == FILE_ATTACHMENT.strip_prefix('#')
}

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

/// One file attachment of the Outlook draft: its id, and the cleaned name and listed size it
/// is matched by.
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

/// What a reconcile changes: the listed attachments to delete, Rail's to upload (indexes into
/// the message's attachments), and for each of Rail's the listed one it matched (an index into
/// the listing), `None` for one to upload.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Changes {
    pub delete: Vec<String>,
    pub upload: Vec<usize>,
    pub matched: Vec<Option<usize>>,
}

/// Rail's attachments matched to the draft's by cleaned name and size, in order, each listed
/// one at most once. The size is the one Outlook listed for it when Rail recorded one (Graph
/// lists a file larger than its bytes), else its bytes (a carried original's: Outlook's).
pub(super) fn changes(listed: &[Listed], wanted: &[OutgoingAttachment]) -> Changes {
    let mut taken = vec![false; listed.len()];
    let (mut upload, mut matched) = (Vec::new(), Vec::new());
    for (index, attachment) in wanted.iter().enumerate() {
        let name = sanitize_filename(&attachment.name);
        let size = attachment.listed.unwrap_or_else(|| attachment.size());
        let found = listed
            .iter()
            .zip(&taken)
            .position(|(l, done)| !done && l.name == name && l.size == size);
        if let Some(done) = found.and_then(|i| taken.get_mut(i)) {
            *done = true;
        } else {
            upload.push(index);
        }
        matched.push(found);
    }
    let delete = listed
        .iter()
        .zip(&taken)
        .filter(|(_, done)| !**done)
        .map(|(l, _)| l.id.clone())
        .collect();
    Changes {
        delete,
        upload,
        matched,
    }
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

/// An attachment Graph answers a post or a finished upload with: its id and listed size.
#[derive(Deserialize)]
struct UploadedWire {
    id: String,
    size: Option<i64>,
}

/// The id and listed size in an upload's answer; `None` when it names no attachment.
pub(super) fn uploaded_of(answer: Option<&[u8]>) -> Option<(String, usize)> {
    let wire: UploadedWire = serde_json::from_slice(answer?).ok()?;
    let size = usize::try_from(wire.size?).ok()?;
    Some((wire.id, size))
}

/// Fill in the listed size of each upload its answer left unknown from `relisted`: the first
/// file of the same cleaned name that no other of Rail's attachments holds.
fn fill_unknown(
    sizes: &mut [Option<usize>],
    held: &mut Vec<String>,
    names: &[String],
    relisted: &[Listed],
) {
    for (size, name) in sizes.iter_mut().zip(names) {
        if size.is_some() {
            continue;
        }
        let found = relisted
            .iter()
            .find(|l| l.name == *name && !held.contains(&l.id));
        if let Some(l) = found {
            *size = Some(l.size);
            held.push(l.id.clone());
        }
    }
}

impl GraphProvider {
    /// Make `draft`'s attachments `mail`'s; the size Outlook lists for each of Rail's, in order
    /// (`None` for one it did not say). `draft` passed `graph_id` when it was saved.
    pub(super) async fn reconcile(
        &self,
        draft: &str,
        mail: &OutgoingMail,
    ) -> Result<Vec<Option<usize>>, ProviderError> {
        let listing = self.file_attachments(draft).await?;
        let attachments = &mail.outgoing.attachments;
        let changes = changes(&listing, attachments);
        for id in &changes.delete {
            self.delete_attachment(draft, id).await?;
        }
        let mut sizes: Vec<Option<usize>> = Vec::with_capacity(attachments.len());
        let mut held = Vec::new();
        for found in &changes.matched {
            let kept = found.and_then(|i| listing.get(i));
            sizes.push(kept.map(|l| l.size));
            held.extend(kept.map(|l| l.id.clone()));
        }
        for &index in &changes.upload {
            let Some(attachment) = attachments.get(index) else {
                continue;
            };
            if let Some((id, size)) = self.upload(draft, attachment).await? {
                held.push(id);
                if let Some(slot) = sizes.get_mut(index) {
                    *slot = Some(size);
                }
            }
        }
        if sizes.iter().any(Option::is_none) {
            let names: Vec<String> = attachments
                .iter()
                .map(|a| sanitize_filename(&a.name))
                .collect();
            let relisted = self.file_attachments(draft).await?;
            fill_unknown(&mut sizes, &mut held, &names, &relisted);
        }
        tracing::info!(
            draft,
            kept = attachments.len() - changes.upload.len(),
            deleted = changes.delete.len(),
            uploaded = changes.upload.len(),
            "a draft's attachments were reconciled"
        );
        Ok(sizes)
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
            .filter(|a| plain_file(a.odata_type.as_deref(), a.is_inline))
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
    /// from its message first. The id and listed size the answer gives, if it gives them.
    async fn upload(
        &self,
        draft: &str,
        attachment: &OutgoingAttachment,
    ) -> Result<Option<(String, usize)>, ProviderError> {
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
    /// The id and listed size Graph answers with.
    async fn upload_small(
        &self,
        draft: &str,
        attachment: &OutgoingAttachment,
        bytes: &[u8],
    ) -> Result<Option<(String, usize)>, ProviderError> {
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
            .map(|answer| uploaded_of(answer.as_deref()))
    }
}
