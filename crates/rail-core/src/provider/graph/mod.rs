//! Microsoft 365 through Microsoft Graph v1.0, over the shared `ApiClient` in its Graph dialect
//! (immutable ids on every request, Graph's error envelope, at most `GRAPH_CONCURRENCY`
//! requests in flight): folders, search and envelopes so far. Graph's own JSON never leaves this module; every string
//! it returns is outside text.

mod batch;
mod dates;
pub mod errors;
mod folders;
mod message;
mod search;
mod wire;

#[cfg(test)]
mod errors_tests;
#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod folders_tests;
#[cfg(test)]
mod message_tests;
#[cfg(test)]
mod search_tests;

use super::http::ApiClient;
use super::{
    Body, ChangeSet, DraftIds, EnvelopePage, IdPage, PageToken, Profile, ProviderError,
    RemoteFolder, RemoteId, SentReceipt, SyncCursor, Window,
};
use crate::ops::M365_NOT_YET;

pub use batch::MAX_BATCH;
pub use folders::{MAX_FOLDER_DEPTH, MAX_FOLDERS, SKIPPED_ROOTS, WELL_KNOWN};
pub use search::{MAX_GRAPH_SEARCH, MESSAGE_FIELDS};

/// The API root every request is relative to; `me` is the signed-in mailbox.
pub const GRAPH_API: &str = "https://graph.microsoft.com/v1.0/";
/// Requests in flight at once for one mailbox: Outlook's limit per app and mailbox.
pub const GRAPH_CONCURRENCY: u32 = 4;
/// Messages per delta page Rail asks for (`Prefer: odata.maxpagesize`).
pub const DELTA_PAGE: u32 = 100;
/// The longest Graph id Rail puts in a path.
pub const MAX_GRAPH_ID_BYTES: usize = 512;
/// Quota units per request: Graph counts requests (10,000 per 10 minutes per app and mailbox).
pub(crate) const COST_REQUEST: u32 = 1;

/// Microsoft 365 through Graph.
pub struct GraphProvider {
    api: ApiClient,
}

impl GraphProvider {
    pub fn new(api: ApiClient) -> Self {
        Self { api }
    }
}

/// Part A's answer for everything Microsoft 365 cannot do yet.
fn not_yet() -> ProviderError {
    ProviderError::permanent(M365_NOT_YET)
}

/// Whether `text` is a Graph id Rail puts in a URL path: 1 to `MAX_GRAPH_ID_BYTES` bytes of
/// ASCII letters, digits, `-`, `_` and `=` (Graph's URL-safe base64).
pub(crate) fn graph_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= MAX_GRAPH_ID_BYTES
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_=".contains(&b))
}

#[async_trait::async_trait]
impl super::Provider for GraphProvider {
    async fn folders(&self) -> Result<Vec<RemoteFolder>, ProviderError> {
        self.list_folders().await
    }

    async fn profile(&self) -> Result<Profile, ProviderError> {
        Err(not_yet())
    }

    async fn list(&self, _: &Window, _: Option<&PageToken>) -> Result<IdPage, ProviderError> {
        Err(not_yet())
    }

    fn list_page_size(&self) -> usize {
        DELTA_PAGE as usize
    }

    async fn changes(&self, _: &SyncCursor) -> Result<ChangeSet, ProviderError> {
        Err(not_yet())
    }

    async fn body(&self, _: &str) -> Result<Body, ProviderError> {
        Err(not_yet())
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<RemoteId>, ProviderError> {
        self.search_ids(query, limit).await
    }

    async fn envelopes(&self, ids: &[RemoteId]) -> Result<EnvelopePage, ProviderError> {
        self.envelopes_of(ids).await
    }

    async fn modify_threads(
        &self,
        _: &[RemoteId],
        _: &[RemoteId],
        _: &[RemoteId],
    ) -> Result<(), ProviderError> {
        Err(not_yet())
    }

    async fn trash_threads(&self, _: &[RemoteId]) -> Result<(), ProviderError> {
        Err(not_yet())
    }

    async fn send(&self, _: &[u8], _: Option<&str>) -> Result<SentReceipt, ProviderError> {
        Err(not_yet())
    }

    async fn message_by_header_id(&self, _: &str) -> Result<Option<RemoteId>, ProviderError> {
        Err(not_yet())
    }

    async fn attachment(&self, _: &RemoteId, _: &str) -> Result<Vec<u8>, ProviderError> {
        Err(not_yet())
    }

    async fn draft_create(&self, _: &[u8], _: Option<&str>) -> Result<DraftIds, ProviderError> {
        Err(not_yet())
    }

    async fn draft_update(
        &self,
        _: &str,
        _: &[u8],
        _: Option<&str>,
    ) -> Result<DraftIds, ProviderError> {
        Err(not_yet())
    }

    async fn draft_delete(&self, _: &str) -> Result<(), ProviderError> {
        Err(not_yet())
    }

    async fn draft_find(&self, _: &str) -> Result<Option<RemoteId>, ProviderError> {
        Err(not_yet())
    }
}
