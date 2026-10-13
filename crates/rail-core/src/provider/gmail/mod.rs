//! The Gmail provider: labels, `messages.list` and `messages.get`, history and search, thread
//! changes (`threads.modify`, `threads.trash`), over the shared `ApiClient`. Every string Gmail returns is outside text.

pub(crate) mod actions;
mod attachment;
pub(crate) mod body;
mod drafts;
mod history;
mod labels;
pub(crate) mod list;
pub(crate) mod metadata;
pub mod mime;
mod send;
mod wire;
mod write;

#[cfg(test)]
mod actions_tests;
#[cfg(test)]
mod attachment_tests;
#[cfg(test)]
mod body_tests;
#[cfg(test)]
mod drafts_tests;
#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod mime_tests;
#[cfg(test)]
mod send_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod write_tests;

use super::http::{ApiClient, is_bad_request};
use super::{
    Body, Change, ChangeSet, DraftIds, EnvelopePage, IdPage, OutgoingMail, PageToken, Profile,
    ProviderError, RemoteFolder, RemoteId, SentReceipt, SyncCursor, ThreadTarget, Window,
};

pub use body::{MAX_BODY_BYTES, TRUNCATED_MARKER};
pub use drafts::{DRAFT_LIST_PAGE, MAX_DRAFT_LIST_PAGES};
pub use history::{MAX_CHANGES_ADDED, MAX_HISTORY_PAGES, MAX_HISTORY_RECORDS};

pub use metadata::{
    MAX_ADDRESSES, MAX_HEADER_BYTES, MAX_SNIPPET_CHARS, MAX_SUBJECT_CHARS, METADATA_HEADERS,
};
pub use mime::{MAX_MESSAGE_BYTES, build as build_message, sanitize_filename};

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
pub(crate) const COST_THREAD_MODIFY: u32 = 10;
pub(crate) const COST_THREAD_TRASH: u32 = 20;
pub(crate) const COST_MESSAGE_SEND: u32 = 100;
pub(crate) const COST_DRAFT_CREATE: u32 = 10;
pub(crate) const COST_DRAFT_UPDATE: u32 = 15;
pub(crate) const COST_DRAFT_DELETE: u32 = 10;
pub(crate) const COST_DRAFT_LIST: u32 = 5;
/// Gmail's limit on each of `addLabelIds` and `removeLabelIds`.
pub const MAX_LABELS_PER_MODIFY: usize = 100;

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
            messages_total: Some(profile.messages_total),
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

    fn list_page_size(&self) -> usize {
        BACKFILL_PAGE as usize
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

    async fn apply(&self, threads: &[ThreadTarget], change: &Change) -> Result<(), ProviderError> {
        self.apply_change(threads, change).await
    }

    async fn send(&self, mail: &OutgoingMail) -> Result<SentReceipt, ProviderError> {
        self.send_mail(mail).await
    }

    async fn draft_save(&self, mail: &OutgoingMail) -> Result<DraftIds, ProviderError> {
        self.save_draft(mail).await
    }

    async fn sent_already(&self, mail: &OutgoingMail) -> Result<bool, ProviderError> {
        self.was_sent(mail).await
    }

    /// Gmail's send makes a new message: a saved draft of it stays until Rail deletes it.
    fn send_consumes_draft(&self) -> bool {
        false
    }

    /// Gmail sends the bytes Rail built: a forward's originals are read first.
    fn forward_carries_originals(&self) -> bool {
        false
    }

    async fn attachment(&self, message: &RemoteId, part: &str) -> Result<Vec<u8>, ProviderError> {
        self.attachment_bytes(message, part).await
    }

    async fn draft_delete(&self, draft_id: &str) -> Result<(), ProviderError> {
        self.delete_draft(draft_id).await
    }

    async fn draft_find(&self, message_id: &str) -> Result<Option<RemoteId>, ProviderError> {
        self.find_draft(message_id).await
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

/// Timing bounds in this provider's tests are generous for a debug build; an instrumented
/// (`cargo llvm-cov`) build runs several times slower, so they scale by this factor.
#[cfg(test)]
pub(crate) const SLOWDOWN: u32 = if cfg!(coverage) { 4 } else { 1 };

#[cfg(test)]
mod slowdown_tests {
    #[test]
    fn the_slowdown_factor_follows_the_coverage_cfg() {
        let expected = if cfg!(coverage) { 4 } else { 1 };
        eprintln!("SLOWDOWN = {}", super::SLOWDOWN);
        assert_eq!(super::SLOWDOWN, expected);
    }
}
