//! One message's body: `GET me/messages/{id}?$select=id,body,hasAttachments` with
//! `Prefer: outlook.body-content-type="html"`, rendered as Gmail's are (`gmail::body::render`:
//! HTML through `provider::html`, text as it is, both cut at `MAX_BODY_BYTES`). With
//! `hasAttachments`, the attachment list comes with it, inline ones included, as Gmail lists
//! every named part. Graph says `hasAttachments` is false when every attachment is inline, and
//! then none is listed.

use super::wire::{AttachmentWire, BodyWire, Page};
use super::{COST_REQUEST, GraphProvider, graph_id};
use crate::provider::gmail::body::{Kind, render};
use crate::provider::gmail::metadata::one_line_capped;
use crate::provider::{AttachmentMeta, Body, ProviderError};

/// The preference that asks for the HTML body.
pub const HTML_BODY: &str = "outlook.body-content-type=\"html\"";
const MAX_ATTACHMENTS: usize = 100;
const MAX_FILENAME_CHARS: usize = 255;
const MAX_MIME_CHARS: usize = 255;

fn unreadable() -> ProviderError {
    ProviderError::permanent("Rail could not read this message's body")
}

fn meta(wire: AttachmentWire) -> AttachmentMeta {
    let mime = wire
        .content_type
        .as_deref()
        .unwrap_or("application/octet-stream")
        .to_ascii_lowercase();
    AttachmentMeta {
        filename: one_line_capped(
            wire.name.as_deref().unwrap_or("attachment"),
            MAX_FILENAME_CHARS,
        ),
        mime_type: one_line_capped(&mime, MAX_MIME_CHARS),
        size: wire.size.unwrap_or(0).max(0),
        part_id: wire.id,
    }
}

impl GraphProvider {
    pub(crate) async fn read_body(&self, id: &str) -> Result<Body, ProviderError> {
        if !graph_id(id) {
            return Err(unreadable());
        }
        let query = [("$select", "id,body,hasAttachments".to_string())];
        let path = format!("me/messages/{id}");
        let wire: BodyWire = self
            .api
            .get_json_with(&path, &query, COST_REQUEST, &[HTML_BODY])
            .await?;
        if wire.id.as_deref() != Some(id) {
            return Err(unreadable());
        }
        let attachments = match wire.has_attachments {
            Some(true) => self.attachment_list(id).await?,
            _ => Vec::new(),
        };
        let body = wire.body.unwrap_or_default();
        let kind = match body.content_type.as_deref() {
            Some(text) if text.eq_ignore_ascii_case("text") => Kind::Plain,
            _ => Kind::Html,
        };
        let content = body.content.unwrap_or_default();
        tokio::task::spawn_blocking(move || render(kind, content.as_bytes(), None, attachments))
            .await
            .map_err(|_| unreadable())?
    }

    async fn attachment_list(&self, id: &str) -> Result<Vec<AttachmentMeta>, ProviderError> {
        let query = [
            ("$select", "id,name,contentType,size,isInline".to_string()),
            ("$top", MAX_ATTACHMENTS.to_string()),
        ];
        let path = format!("me/messages/{id}/attachments");
        let page: Page<AttachmentWire> = self.api.get_json(&path, &query, COST_REQUEST).await?;
        Ok(page
            .value
            .into_iter()
            .filter(|a| graph_id(&a.id))
            .take(MAX_ATTACHMENTS)
            .map(meta)
            .collect())
    }
}
