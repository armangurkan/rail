//! The send path of a `Send` row, and the one place that decides whether a message may go out
//! again. Before any attempt but the first, and for a row found in flight at start (the
//! recovered set), the worker asks Gmail for the draft's `Message-ID`: found means it was
//! sent, and the row completes as if this send succeeded; not found means it sends, with the
//! same `Message-ID`. `Provider::send` is never retried inside the HTTP layer.

use std::collections::HashSet;
use std::sync::PoisonError;

use super::Worker;
use super::compose::{self, Assembly};
use crate::ops::{DraftId, OutboxId};
use crate::provider::ProviderError;
use crate::provider::gmail::build_message;
use crate::store::outbox::{OutboxOutcome, OutboxRow, OutboxWrite};

/// What a send reads from the store in one go.
struct Loaded {
    message_id: String,
    remote_thread: Option<String>,
    attachments_readable: bool,
    assembly: Assembly,
}

impl Worker {
    /// The restart rule: the account's inflight rows back to pending, remembered so a `Send`
    /// among them is looked up before it is sent.
    pub(super) async fn recover(&self) {
        let recovered = self
            .writer
            .outbox(OutboxWrite::Recover {
                account_id: self.account_id,
            })
            .await;
        match recovered {
            Ok(OutboxOutcome::Recovered(ids)) => {
                self.recovered_ids().extend(ids);
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(account = %self.name, error = %error.message, "could not recover the outbox");
            }
        }
    }

    fn recovered_ids(&self) -> std::sync::MutexGuard<'_, HashSet<OutboxId>> {
        self.recovered
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// The row is settled (sent, failed or back to pending with an attempt counted): the
    /// attempt count carries the rule from here.
    pub(super) fn forget_recovered(&self, id: OutboxId) {
        self.recovered_ids().remove(&id);
    }

    /// True when an earlier attempt of the row may have reached Gmail.
    fn may_have_been_sent(&self, row: &OutboxRow) -> bool {
        row.attempts > 0 || self.recovered_ids().contains(&row.id)
    }

    /// Send the draft of a `Send` row, unless Gmail already holds it.
    pub(super) async fn send_draft(
        &self,
        row: &OutboxRow,
        draft: DraftId,
    ) -> Result<(), ProviderError> {
        let loaded = self.load(draft).await?;
        if self.may_have_been_sent(row)
            && self
                .provider
                .message_by_header_id(&loaded.message_id)
                .await?
                .is_some()
        {
            tracing::info!(account = %self.name, row = row.id, "the message was sent already");
            return Ok(());
        }
        if !loaded.attachments_readable {
            return Err(ProviderError::permanent(
                "the draft's attachments could not be read",
            ));
        }
        let outgoing = compose::attach(loaded.assembly, self.provider.as_ref()).await?;
        let raw = build_message(&outgoing)?;
        self.provider
            .send(&raw, loaded.remote_thread.as_deref())
            .await?;
        Ok(())
    }

    async fn load(&self, draft: DraftId) -> Result<Loaded, ProviderError> {
        let (account, from, from_name) = (
            self.account_id,
            self.address.clone(),
            self.from_name.clone(),
        );
        let loaded = self
            .read(move |store| {
                let Some(d) = store.draft(draft)?.filter(|d| d.account_id == account) else {
                    return Ok(None);
                };
                let attachments_readable = store.draft_attachments_readable(draft)?;
                let assembly = compose::outgoing_for(store, &d, &from, from_name.as_deref())?;
                Ok(Some(Loaded {
                    message_id: d.message_id,
                    remote_thread: d.remote_thread_id,
                    attachments_readable,
                    assembly,
                }))
            })
            .await
            .map_err(|e| ProviderError::transient(&e.message))?;
        loaded.ok_or_else(|| ProviderError::permanent("the draft is gone"))
    }
}
