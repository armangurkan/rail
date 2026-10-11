//! `messages.list` and concurrent `messages.get` with `format=metadata`.

use futures_util::{StreamExt, stream};

use super::wire::MessageList;
use super::{
    COST_GET, COST_LIST, GMAIL_CONCURRENCY, GmailProvider, METADATA_HEADERS, checked_id, metadata,
    unsafe_id,
};
use crate::provider::http::Readable;
use crate::provider::{Envelope, EnvelopePage, PageToken, ProviderError, RemoteId, Window};

/// Gmail's search terms for `window`, in whole seconds; `None` for the whole mailbox.
pub(crate) fn window_query(window: &Window) -> Option<String> {
    let mut terms = Vec::new();
    if let Some(after) = window.after_ms {
        terms.push(format!("after:{}", after.div_euclid(1000)));
    }
    if let Some(before) = window.before_ms {
        terms.push(format!("before:{}", before.div_euclid(1000)));
    }
    (!terms.is_empty()).then(|| terms.join(" "))
}

/// What fetching one message's metadata came to.
enum Fetched {
    Read(Box<Envelope>),
    /// Skipped, with Rail's own words for why (never Gmail's text or the message's).
    Unreadable(RemoteId, String),
    Gone,
}

// Removed in Task 5 when impl Provider lands.
#[cfg_attr(not(test), allow(dead_code))]
impl GmailProvider {
    /// One page of message ids, newest first.
    pub(crate) async fn list_ids(
        &self,
        q: Option<String>,
        page: Option<&PageToken>,
        max: u32,
        include_spam_trash: bool,
    ) -> Result<(Vec<RemoteId>, Option<PageToken>), ProviderError> {
        let mut query = vec![
            ("maxResults", max.to_string()),
            ("includeSpamTrash", include_spam_trash.to_string()),
        ];
        if let Some(q) = q {
            query.push(("q", q));
        }
        if let Some(page) = page {
            query.push(("pageToken", page.0.clone()));
        }
        let list: MessageList = self.api.get_json("messages", &query, COST_LIST).await?;
        let ids = list.messages.into_iter().map(|m| m.id).collect();
        Ok((ids, list.next_page_token.map(PageToken)))
    }

    /// Metadata for `ids`, `GMAIL_CONCURRENCY` requests at a time, in the order given. Ids Rail
    /// cannot read (or would not put in a URL) are skipped and logged by id; ids Gmail no
    /// longer has are dropped. Any other failure fails the whole call.
    pub(crate) async fn metadata(&self, ids: &[RemoteId]) -> Result<EnvelopePage, ProviderError> {
        let fetched: Vec<Result<Fetched, ProviderError>> = stream::iter(ids)
            .map(|id| self.one_metadata(id))
            .buffered(GMAIL_CONCURRENCY)
            .collect()
            .await;
        let mut page = EnvelopePage::default();
        for outcome in fetched {
            match outcome? {
                Fetched::Read(envelope) => page.envelopes.push(*envelope),
                Fetched::Unreadable(id, reason) => {
                    tracing::warn!(
                        id = %id.escape_debug(),
                        reason,
                        "skipped a message Rail could not read"
                    );
                    page.skipped.push(id);
                }
                Fetched::Gone => {}
            }
        }
        Ok(page)
    }

    async fn one_metadata(&self, id: &RemoteId) -> Result<Fetched, ProviderError> {
        if unsafe_id(id) {
            let reason = "its id is not safe in a request path".to_string();
            return Ok(Fetched::Unreadable(id.clone(), reason));
        }
        let mut query = vec![("format", "metadata".to_string())];
        query.extend(
            METADATA_HEADERS
                .iter()
                .map(|h| ("metadataHeaders", (*h).to_string())),
        );
        let path = format!("messages/{}", checked_id(id).unwrap_or_default());
        let message = match self.api.get_readable(&path, &query, COST_GET).await {
            Ok(Readable::Parsed(message)) => message,
            // Over the response cap or not a message: this one is skipped, not the pass.
            // The error's message is Rail's own fixed text.
            Ok(Readable::Unreadable(error)) => {
                return Ok(Fetched::Unreadable(id.clone(), error.to_string()));
            }
            Err(ProviderError::NotFound) => return Ok(Fetched::Gone),
            Err(error) => return Err(error),
        };
        Ok(match metadata::envelope(message) {
            Some(envelope) => Fetched::Read(Box::new(envelope)),
            None => {
                let reason = "it has no id, thread or usable internalDate".to_string();
                Fetched::Unreadable(id.clone(), reason)
            }
        })
    }
}
