//! The Gmail provider: labels, `messages.list` and `messages.get`, history and search, over
//! the shared `ApiClient`. Every string Gmail returns is outside text.

mod body;
mod history;
mod html;
#[cfg(test)]
mod html_tests;
mod labels;
pub(crate) mod list;
pub(crate) mod metadata;
mod wire;

#[cfg(test)]
mod body_tests;
#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tests;

use super::http::{ApiClient, is_bad_request};
use super::{
    Body, ChangeSet, EnvelopePage, IdPage, PageToken, Profile, ProviderError, RemoteFolder,
    RemoteId, SyncCursor, Window,
};

pub use body::{MAX_BODY_BYTES, TRUNCATED_MARKER};
pub use history::{MAX_CHANGES_ADDED, MAX_HISTORY_PAGES, MAX_HISTORY_RECORDS};

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
/// A `historyId` is a decimal `u64`: at most 20 digits.
const MAX_HISTORY_ID_DIGITS: usize = 20;

/// Quota units per call (Gmail quota reference).
pub(crate) const COST_LIST: u32 = 5;
pub(crate) const COST_GET: u32 = 20;
pub(crate) const COST_LABELS: u32 = 1;
pub(crate) const COST_PROFILE: u32 = 1;
pub(crate) const COST_HISTORY: u32 = 2;
pub(crate) const COST_ATTACHMENT: u32 = 5;

/// The most server hits one search asks for.
pub const MAX_SEARCH_RESULTS: usize = 100;

/// Gmail through its REST API.
pub struct GmailProvider {
    api: ApiClient,
}

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
        let id = &profile.history_id;
        if id.is_empty()
            || id.len() > MAX_HISTORY_ID_DIGITS
            || !id.bytes().all(|b| b.is_ascii_digit())
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

    /// One page of the window's ids in Gmail's order (roughly newest first, not strictly).
    /// A 400 to a request carrying `page` is `PageTokenRejected`; a next-page token Rail would
    /// not send back is a permanent error, never a silently shortened listing.
    pub(crate) async fn list_page(
        &self,
        window: &Window,
        page: Option<&PageToken>,
    ) -> Result<IdPage, ProviderError> {
        let query = list::window_query(window);
        let (ids, next) = match self.list_ids(query, page, BACKFILL_PAGE, true).await {
            Err(error) if page.is_some() && is_bad_request(&error) => {
                return Err(ProviderError::PageTokenRejected);
            }
            other => other?,
        };
        let next = match next {
            Some(token) => Some(PageToken::checked(&token.0).ok_or_else(|| {
                ProviderError::permanent("Gmail's next page token is not one Rail sends back")
            })?),
            None => None,
        };
        Ok(IdPage { ids, next })
    }

    /// Gmail's own search over the whole mailbox (spam and trash excluded, as in Gmail). Ids
    /// Rail would not put in a path are left out.
    pub(crate) async fn search_ids(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<RemoteId>, ProviderError> {
        let limit = limit.clamp(1, MAX_SEARCH_RESULTS);
        let max = u32::try_from(limit).unwrap_or(1);
        let (ids, _) = self
            .list_ids(Some(query.to_string()), None, max, false)
            .await?;
        Ok(ids
            .into_iter()
            .filter(|id| checked_id(id).is_some())
            .take(limit)
            .collect())
    }
}

#[async_trait::async_trait]
impl super::Provider for GmailProvider {
    async fn folders(&self) -> Result<Vec<RemoteFolder>, ProviderError> {
        self.labels().await
    }

    async fn profile(&self) -> Result<Profile, ProviderError> {
        self.read_profile().await
    }

    async fn list(
        &self,
        window: &Window,
        page: Option<&PageToken>,
    ) -> Result<IdPage, ProviderError> {
        self.list_page(window, page).await
    }

    async fn changes(&self, cursor: &SyncCursor) -> Result<ChangeSet, ProviderError> {
        self.history(cursor).await
    }

    async fn body(&self, id: &str) -> Result<Body, ProviderError> {
        self.full_body(id).await
    }

    async fn search(&self, query: &str, limit: usize) -> Result<Vec<RemoteId>, ProviderError> {
        self.search_ids(query, limit).await
    }

    async fn envelopes(&self, ids: &[RemoteId]) -> Result<EnvelopePage, ProviderError> {
        self.metadata(ids).await
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
