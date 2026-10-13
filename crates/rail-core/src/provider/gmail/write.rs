//! The writing side of the trait for Gmail: the message is built with the MIME builder
//! (`mime::build`) and goes through the raw-bytes calls, unchanged on the wire. A thread Gmail
//! no longer has (404) makes the send or the create go on its own; an update of a draft deleted
//! at Gmail creates it again. Logs carry no content.

use super::GmailProvider;
use super::mime::build;
use crate::provider::{DraftIds, OutgoingMail, ProviderError, SentReceipt};

/// The message's RFC 5322 bytes. Gmail sends what Rail built, so every attachment must have
/// its bytes: a carried one (Graph's forward) never reaches Gmail.
fn raw_of(mail: &OutgoingMail) -> Result<Vec<u8>, ProviderError> {
    if mail
        .outgoing
        .attachments
        .iter()
        .any(|a| a.carried.is_some())
    {
        return Err(ProviderError::permanent(
            "a forwarded attachment was not read",
        ));
    }
    build(&mail.outgoing)
}

impl GmailProvider {
    pub(crate) async fn send_mail(
        &self,
        mail: &OutgoingMail,
    ) -> Result<SentReceipt, ProviderError> {
        let raw = raw_of(mail)?;
        let thread = mail.thread.as_deref();
        match self.send_raw(&raw, thread).await {
            // A thread deleted at Gmail is refused with 404 before anything is accepted, so
            // sending once more on a thread of its own cannot make a duplicate.
            Err(ProviderError::NotFound) if thread.is_some() => {
                tracing::info!("the reply's thread is gone at Gmail; sending it on its own");
                self.send_raw(&raw, None).await
            }
            other => other,
        }
    }

    pub(crate) async fn save_draft(&self, mail: &OutgoingMail) -> Result<DraftIds, ProviderError> {
        let raw = raw_of(mail)?;
        let thread = mail.thread.as_deref();
        if let Some(remote) = &mail.remote {
            match self.update_draft(&remote.draft_id, &raw, thread).await {
                Err(ProviderError::NotFound) => {
                    tracing::info!("the Gmail draft is gone; saving it again");
                }
                other => return other,
            }
        }
        self.create_in_thread(&raw, thread).await
    }

    /// A create; one in a thread Gmail no longer has is saved on its own (a 404 is a refusal
    /// before anything is saved, so this cannot make two).
    async fn create_in_thread(
        &self,
        raw: &[u8],
        thread: Option<&str>,
    ) -> Result<DraftIds, ProviderError> {
        match self.create_draft(raw, thread).await {
            Err(ProviderError::NotFound) if thread.is_some() => {
                tracing::info!("the draft's thread is gone at Gmail; saving it on its own");
                self.create_draft(raw, None).await
            }
            other => other,
        }
    }

    pub(crate) async fn was_sent(&self, mail: &OutgoingMail) -> Result<bool, ProviderError> {
        Ok(self
            .find_by_header_id(&mail.outgoing.message_id)
            .await?
            .is_some())
    }
}
