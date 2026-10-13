//! One chunk of an upload session (Microsoft Graph's large attachments): a `PUT` of bytes to
//! the session's own address, which carries its own credential, so neither `Authorization` nor
//! `Prefer` goes with it. The address is never logged and never put in an error: the log has
//! the status and the chunk's size. A chunk takes one of the account's request permits and one
//! quota unit and is tried once: the caller resumes from what the session says it has.

use reqwest::header::{CONTENT_RANGE, CONTENT_TYPE};
use url::Url;

use super::{ApiClient, Attempt, MAX_ERROR_BODY, classify, read_capped, transport};
use crate::provider::ProviderError;

/// Where an upload session may live besides the API's own host: Outlook's domains.
const UPLOAD_DOMAINS: [&str; 2] = [".office.com", ".office365.com"];

/// A 2xx answer to a chunk: its status (200: more expected; 201: the attachment is made) and
/// its body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChunkAnswer {
    pub status: u16,
    pub body: Vec<u8>,
}

fn not_used() -> ProviderError {
    ProviderError::permanent("Microsoft 365 answered an upload address Rail does not use")
}

impl ApiClient {
    /// `text` as an upload address Rail sends bytes to: the API's scheme, no user or password,
    /// and the API's own host or one under `office.com` or `office365.com`.
    pub(crate) fn upload_url(&self, text: &str) -> Result<Url, ProviderError> {
        let url = Url::parse(text).map_err(|_| not_used())?;
        let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
        let known = Some(host.as_str()) == self.base.host_str()
            || UPLOAD_DOMAINS.iter().any(|domain| host.ends_with(domain));
        let plain = url.username().is_empty() && url.password().is_none();
        if host.is_empty() || url.scheme() != self.base.scheme() || !known || !plain {
            return Err(not_used());
        }
        Ok(url)
    }

    /// `PUT` `chunk`, the bytes from `start` of `total`, to the session at `url`.
    pub(crate) async fn put_upload_chunk(
        &self,
        url: &Url,
        start: usize,
        chunk: &[u8],
        total: usize,
    ) -> Result<ChunkAnswer, ProviderError> {
        let _permit = self.permit(1).await?;
        self.quota.take(1).await;
        let last = start + chunk.len().saturating_sub(1);
        let name = self.dialect.name();
        let response = self
            .http
            .put(url.clone())
            .header(CONTENT_RANGE, format!("bytes {start}-{last}/{total}"))
            .header(CONTENT_TYPE, "application/octet-stream")
            .body(chunk.to_vec())
            .send()
            .await
            .map_err(|e| transport(name, e))?;
        let status = response.status();
        tracing::debug!(
            status = status.as_u16(),
            bytes = chunk.len(),
            "upload chunk"
        );
        let body = read_capped(response, MAX_ERROR_BODY, name)
            .await?
            .unwrap_or_default();
        if status.is_success() {
            return Ok(ChunkAnswer {
                status: status.as_u16(),
                body,
            });
        }
        Err(match classify(self.dialect, status, None, body) {
            Attempt::Failed(error) => error,
            _ => ProviderError::transient("Microsoft 365's upload session is no longer valid"),
        })
    }
}
