//! Mail providers: the read side of the `Provider` trait, the types it speaks, the HTTP layer
//! providers share, and the Gmail implementation. Actions, attachments and sends join the
//! trait in Phase 6.

pub mod error;
pub mod gmail;
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
    AttachmentMeta, Body, ChangeSet, Envelope, EnvelopePage, FolderKind, IdPage,
    MAX_PAGE_TOKEN_BYTES, Mailbox, PageToken, Profile, Relabel, RemoteFolder, RemoteId, SyncCursor,
    Window,
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
    /// What changed since `cursor`. `ProviderError::CursorExpired` means a full resync.
    async fn changes(&self, cursor: &SyncCursor) -> Result<ChangeSet, ProviderError>;
    /// One message's body, rendered to text.
    async fn body(&self, id: &str) -> Result<Body, ProviderError>;
    /// The provider's own search over the whole mailbox: matching message ids.
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<RemoteId>, ProviderError>;
    /// Envelopes for these ids; ids that no longer exist are left out.
    async fn envelopes(&self, ids: &[RemoteId]) -> Result<EnvelopePage, ProviderError>;
}
