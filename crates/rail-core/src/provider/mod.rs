//! Mail providers: the read side of the `Provider` trait, the types it speaks, the HTTP layer
//! providers share, and the Gmail and Microsoft Graph implementations.

pub mod error;
pub mod gmail;
pub mod graph;
pub(crate) mod html;
#[cfg(test)]
mod html_tests;
pub mod http;
pub mod links;
#[cfg(test)]
mod links_tests;
pub mod tokens;
pub mod types;

#[cfg(test)]
pub(crate) mod mock;

pub use error::ProviderError;
pub use types::{
    AttachmentMeta, Body, Change, ChangeSet, DraftIds, Envelope, EnvelopePage, FolderKind,
    INBOX_LABEL, IdPage, MAX_PAGE_TOKEN_BYTES, Mailbox, Outgoing, OutgoingAttachment, PageToken,
    Profile, Relabel, RemoteFolder, RemoteId, STARRED_LABEL, SentReceipt, SyncCursor,
    TargetMessage, ThreadTarget, UNREAD_LABEL, Window,
};

/// One account's mailbox, read through its provider's API. Every method speaks Rail types;
/// object-safe so the daemon can keep one `Arc<dyn Provider>` per account.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Every folder (Gmail: label) of the mailbox.
    async fn folders(&self) -> Result<Vec<RemoteFolder>, ProviderError>;
    /// The mailbox's current sync cursor and size, read before a backfill.
    async fn profile(&self) -> Result<Profile, ProviderError>;
    /// One page of the message ids inside `window`, in the provider's order (Gmail: roughly
    /// newest first, not strictly). `page` must come from a listing of the same `window`;
    /// `ProviderError::PageTokenRejected` means the provider refused it.
    async fn list(
        &self,
        window: &Window,
        page: Option<&PageToken>,
    ) -> Result<IdPage, ProviderError>;
    /// How many ids one `list` page holds at most.
    fn list_page_size(&self) -> usize;
    /// True when the provider's first sync is its delta (Graph): `list` finds nothing, the
    /// full backfill's sweep waits until that delta has caught up, and a grown window starts
    /// the delta over.
    fn backfills_by_delta(&self) -> bool {
        false
    }
    /// What changed since `cursor`. `ProviderError::CursorExpired` means a full resync.
    async fn changes(&self, cursor: &SyncCursor) -> Result<ChangeSet, ProviderError>;
    /// One message's body, rendered to text.
    async fn body(&self, id: &str) -> Result<Body, ProviderError>;
    /// The provider's own search over the whole mailbox: matching message ids.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<RemoteId>, ProviderError>;
    /// Envelopes for these ids; ids that no longer exist are left out.
    async fn envelopes(&self, ids: &[RemoteId]) -> Result<EnvelopePage, ProviderError>;
    /// Add and remove labels (system ones too: `INBOX_LABEL`, `UNREAD_LABEL`, `STARRED_LABEL`)
    /// on every message of each thread, one thread at a time. Idempotent. Each target carries
    /// the ids of the thread's stored messages, for a provider that acts on messages.
    async fn modify_threads(
        &self,
        threads: &[ThreadTarget],
        add: &[RemoteId],
        remove: &[RemoteId],
    ) -> Result<(), ProviderError>;
    /// Move each thread to the provider's trash (restorable there). Idempotent.
    async fn trash_threads(&self, threads: &[ThreadTarget]) -> Result<(), ProviderError>;
    /// Send the RFC 5322 message `raw`, in `thread_id` when given. NEVER retried blindly: a
    /// request that may have reached the provider is not repeated, so a transient error here
    /// means "unknown"; the caller looks the message up (`message_by_header_id`) before it
    /// sends again.
    async fn send(&self, raw: &[u8], thread_id: Option<&str>)
    -> Result<SentReceipt, ProviderError>;
    /// The message whose `Message-ID` header is `rfc822_message_id` (angle brackets
    /// included), if the mailbox holds it: the "was it sent already" check.
    async fn message_by_header_id(
        &self,
        rfc822_message_id: &str,
    ) -> Result<Option<RemoteId>, ProviderError>;
    /// The bytes of one attachment part of a message.
    async fn attachment(&self, message: &RemoteId, part: &str) -> Result<Vec<u8>, ProviderError>;
    /// Save the RFC 5322 message `raw` as a draft, in `thread_id` when given. Tried once: a
    /// transient error means unknown (the draft may exist), and the caller decides.
    async fn draft_create(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<DraftIds, ProviderError>;
    /// Replace the provider draft's message with `raw`. `ProviderError::NotFound` when the
    /// draft was deleted at the provider.
    async fn draft_update(
        &self,
        draft_id: &str,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<DraftIds, ProviderError>;
    /// Delete the provider draft. `ProviderError::NotFound` when it is gone already.
    async fn draft_delete(&self, draft_id: &str) -> Result<(), ProviderError>;
    /// The provider draft whose message is `message_id`, looked for in at most
    /// `gmail::MAX_DRAFT_LIST_PAGES` pages of the draft listing.
    async fn draft_find(&self, message_id: &str) -> Result<Option<RemoteId>, ProviderError>;
}
