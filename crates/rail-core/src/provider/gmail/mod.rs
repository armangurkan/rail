//! The Gmail provider: labels, `messages.list` and `messages.get`, history and search, over
//! the shared `ApiClient`. Every string Gmail returns is outside text.

// Only tests call the Gmail methods until `impl Provider for GmailProvider` lands; the
// `dead_code` allowances below are removed in Task 5 when impl Provider lands.
#[cfg_attr(not(test), allow(dead_code))]
mod labels;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod list;
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) mod metadata;
mod wire;

#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod tests;

use super::http::ApiClient;
use super::{EnvelopePage, PageToken, Profile, ProviderError, RemoteFolder, SyncCursor, Window};

pub use metadata::{
    MAX_ADDRESSES, MAX_HEADER_BYTES, MAX_SNIPPET_CHARS, MAX_SUBJECT_CHARS, METADATA_HEADERS,
};

/// The API root every request is relative to; `users/me` is the authorised account.
pub const GMAIL_API: &str = "https://gmail.googleapis.com/gmail/v1/users/me/";
/// `messages.get` requests in flight at once.
pub const GMAIL_CONCURRENCY: usize = 8;
/// Message ids per backfill page (and so per writer batch).
pub const BACKFILL_PAGE: u32 = 100;
/// The longest message id Rail puts in a path.
const MAX_ID_BYTES: usize = 64;

/// Quota units per call (Gmail quota reference).
pub(crate) const COST_LIST: u32 = 5;
pub(crate) const COST_GET: u32 = 20;
pub(crate) const COST_LABELS: u32 = 1;
pub(crate) const COST_PROFILE: u32 = 1;

/// Gmail through its REST API.
pub struct GmailProvider {
    api: ApiClient,
}

#[cfg_attr(not(test), allow(dead_code))]
impl GmailProvider {
    pub fn new(api: ApiClient) -> Self {
        Self { api }
    }

    pub(crate) async fn labels(&self) -> Result<Vec<RemoteFolder>, ProviderError> {
        let list: wire::LabelList = self.api.get_json("labels", &[], COST_LABELS).await?;
        Ok(list.labels.into_iter().filter_map(labels::folder).collect())
    }

    pub(crate) async fn read_profile(&self) -> Result<Profile, ProviderError> {
        let profile: wire::ProfileWire = self.api.get_json("profile", &[], COST_PROFILE).await?;
        if profile.history_id.is_empty() || !profile.history_id.bytes().all(|b| b.is_ascii_digit())
        {
            return Err(ProviderError::permanent(
                "Gmail's profile has no usable history id",
            ));
        }
        Ok(Profile {
            cursor: SyncCursor(profile.history_id),
            messages_total: profile.messages_total,
        })
    }

    /// One page of the window, newest first, with each message's metadata.
    pub(crate) async fn backfill_page(
        &self,
        window: &Window,
        page: Option<&PageToken>,
    ) -> Result<EnvelopePage, ProviderError> {
        let query = list::window_query(window);
        let (ids, next) = self.list_ids(query, page, BACKFILL_PAGE, true).await?;
        let mut fetched = self.metadata(&ids).await?;
        fetched.next = next;
        Ok(fetched)
    }
}

/// Whether `text` is safe in a URL path segment: 1 to `max` bytes of ASCII letters, digits,
/// `-` and `_`. The one byte check every Gmail-supplied id goes through.
pub(super) fn safe_id(text: &str, max: usize) -> bool {
    !text.is_empty()
        && text.len() <= max
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A message id Gmail sent, if it is safe in a URL path.
pub(super) fn checked_id(id: &str) -> Option<&str> {
    safe_id(id, MAX_ID_BYTES).then_some(id)
}

/// Ids Rail would not put in a path.
fn unsafe_id(id: &str) -> bool {
    checked_id(id).is_none()
}
