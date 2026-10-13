//! An attachment of `SMALL_ATTACHMENT_BYTES` or more goes up through an upload session:
//! `createUploadSession` on the draft, then `PUT`s of `UPLOAD_CHUNK` bytes (twelve 320 KiB
//! blocks; the last is shorter) to the session's own address, one at a time. A failed or
//! unreadable chunk is sent again from where the session last said it was
//! (`nextExpectedRanges`); after `UPLOAD_TRIES` failures the call is transient and the outbox row
//! tries again later with a new session. An unfinished session leaves no attachment on the
//! draft. Logs carry the draft's id, offsets and codes.

use serde::Deserialize;
use serde_json::json;
use url::Url;

use super::drafts::unexpected;
use super::reconcile::uploaded_of;
use super::{COST_REQUEST, GraphProvider};
use crate::provider::gmail::mime::sanitize_filename;
use crate::provider::http::ChunkAnswer;
use crate::provider::{OutgoingAttachment, ProviderError};

/// Bytes per chunk: twelve 320 KiB blocks, as Graph asks for multiples of 320 KiB.
pub const UPLOAD_CHUNK: usize = 12 * 320 * 1024;
/// Failed chunks one session takes before the call is transient.
pub const UPLOAD_TRIES: u32 = 3;

#[derive(Deserialize)]
struct SessionWire {
    #[serde(rename = "uploadUrl")]
    upload_url: String,
}

#[derive(Deserialize)]
struct ProgressWire {
    #[serde(rename = "nextExpectedRanges", alias = "NextExpectedRanges", default)]
    next_expected_ranges: Vec<String>,
}

/// What a chunk's answer says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Progress {
    /// 201: the attachment is made.
    Done,
    /// 200: the next chunk starts here.
    At(usize),
    Unreadable,
}

/// A 201 made the attachment; a 200 names where the next chunk starts, which must be past
/// `offset` and before `total`.
pub(super) fn progress(answer: &ChunkAnswer, offset: usize, total: usize) -> Progress {
    if answer.status == 201 {
        return Progress::Done;
    }
    let next = serde_json::from_slice::<ProgressWire>(&answer.body)
        .ok()
        .and_then(|wire| wire.next_expected_ranges.into_iter().next())
        .and_then(|range| {
            range
                .split('-')
                .next()
                .and_then(|s| s.parse::<usize>().ok())
        });
    match next {
        Some(next) if answer.status == 200 && next > offset && next < total => Progress::At(next),
        _ => Progress::Unreadable,
    }
}

impl GraphProvider {
    /// Upload `bytes` through a session; the id and listed size the last answer gives, if it
    /// gives them (Graph's usually has no body).
    pub(super) async fn upload_large(
        &self,
        draft: &str,
        attachment: &OutgoingAttachment,
        bytes: &[u8],
    ) -> Result<Option<(String, usize)>, ProviderError> {
        let url = self.upload_session(draft, attachment, bytes.len()).await?;
        let total = bytes.len();
        let (mut offset, mut failures) = (0, 0);
        loop {
            let end = (offset + UPLOAD_CHUNK).min(total);
            let chunk = bytes.get(offset..end).unwrap_or_default();
            match self.api.put_upload_chunk(&url, offset, chunk, total).await {
                Ok(answer) => match progress(&answer, offset, total) {
                    Progress::Done => return Ok(uploaded_of(Some(&answer.body))),
                    Progress::At(next) => {
                        offset = next;
                        continue;
                    }
                    Progress::Unreadable => {
                        tracing::info!(draft, offset, "an upload answer was unreadable");
                    }
                },
                Err(error) => {
                    tracing::info!(
                        draft,
                        offset,
                        code = error.code(),
                        "an attachment chunk failed"
                    );
                }
            }
            failures += 1;
            if failures >= UPLOAD_TRIES {
                return Err(ProviderError::transient(
                    "an attachment could not be uploaded to Microsoft 365",
                ));
            }
        }
    }

    async fn upload_session(
        &self,
        draft: &str,
        attachment: &OutgoingAttachment,
        size: usize,
    ) -> Result<Url, ProviderError> {
        let body = json!({"AttachmentItem": {
            "attachmentType": "file",
            "name": sanitize_filename(&attachment.name),
            "size": size,
            "contentType": attachment.mime,
        }});
        let path = format!("me/messages/{draft}/attachments/createUploadSession");
        let answer = self
            .api
            .post_json_bytes(&path, Some(&body), COST_REQUEST, true)
            .await?;
        let wire: SessionWire = answer
            .as_deref()
            .and_then(|bytes| serde_json::from_slice(bytes).ok())
            .ok_or_else(unexpected)?;
        self.api.upload_url(&wire.upload_url)
    }
}
