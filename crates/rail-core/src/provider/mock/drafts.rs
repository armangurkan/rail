//! The mock's writing side: Gmail's raw calls (`messages.send`, the `rfc822msgid:` search,
//! `drafts.create` and `drafts.update`) played on a map of drafts, and the trait's
//! `OutgoingMail` methods made of them as Gmail's adapter makes them (the MIME builder; a
//! thread gone sends or saves on its own; an update of a draft gone creates it). With
//! `graph_model` a send consumes the draft it names, as Graph's does.

use super::{Call, Mailbox, MockDraft, MockProvider, SentRaw};
use crate::provider::gmail::build_message;
use crate::provider::{DraftIds, OutgoingMail, ProviderError, RemoteId, SentReceipt};

/// A new draft; one naming a thread deleted at Gmail (`missing_threads`) is `NotFound`, as a
/// send is, and saves nothing.
pub(super) fn create(
    m: &mut Mailbox,
    raw: &[u8],
    thread: Option<&str>,
) -> Result<DraftIds, ProviderError> {
    if thread.is_some_and(|t| m.missing_threads.contains(t)) {
        return Err(ProviderError::NotFound);
    }
    m.next_draft += 1;
    let ids = DraftIds {
        draft_id: format!("r{}", m.next_draft),
        message_id: format!("dm{}", m.next_draft),
    };
    m.drafts.insert(
        ids.draft_id.clone(),
        MockDraft {
            message_id: ids.message_id.clone(),
            raw: raw.to_vec(),
            thread_id: thread.map(str::to_string),
        },
    );
    Ok(ids)
}

pub(super) fn update(
    m: &mut Mailbox,
    id: &str,
    raw: &[u8],
    thread: Option<&str>,
) -> Result<DraftIds, ProviderError> {
    if m.missing_drafts.contains(id) || !m.drafts.contains_key(id) {
        return Err(ProviderError::NotFound);
    }
    m.next_draft += 1;
    let message_id = format!("dm{}", m.next_draft);
    m.drafts.insert(
        id.to_string(),
        MockDraft {
            message_id: message_id.clone(),
            raw: raw.to_vec(),
            thread_id: thread.map(str::to_string),
        },
    );
    Ok(DraftIds {
        draft_id: id.to_string(),
        message_id,
    })
}

pub(super) fn delete(m: &mut Mailbox, id: &str) -> Result<(), ProviderError> {
    if m.missing_drafts.contains(id) {
        return Err(ProviderError::NotFound);
    }
    m.drafts
        .remove(id)
        .map(|_| ())
        .ok_or(ProviderError::NotFound)
}

pub(super) fn find(m: &Mailbox, message: &str) -> Option<RemoteId> {
    m.drafts
        .iter()
        .find(|(_, d)| d.message_id == message)
        .map(|(id, _)| id.clone())
}

impl MockProvider {
    /// Gmail's `messages.send` of `raw`, played on the mailbox and recorded.
    pub(crate) async fn send_raw(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<SentReceipt, ProviderError> {
        self.enter(Call::Send, None)?;
        let delay = self.lock().action_delay;
        tokio::time::sleep(delay).await;
        let mut m = self.lock();
        if thread_id.is_some_and(|t| m.missing_threads.contains(t)) {
            return Err(ProviderError::NotFound);
        }
        m.sent.push(SentRaw {
            raw: raw.to_vec(),
            thread_id: thread_id.map(str::to_string),
        });
        if let Some(id) = header_message_id(raw) {
            m.known_header_ids.insert(id);
        }
        let n = m.sent.len();
        Ok(SentReceipt {
            message_id: Some(format!("sent-{n}")),
            thread_id: Some(thread_id.map_or_else(|| format!("t-sent-{n}"), str::to_string)),
        })
    }

    /// Gmail's `rfc822msgid:` search.
    pub(crate) async fn find_header(
        &self,
        rfc822_message_id: &str,
    ) -> Result<Option<RemoteId>, ProviderError> {
        self.enter(Call::FindByHeader, None)?;
        let m = self.lock();
        Ok(m.known_header_ids
            .contains(rfc822_message_id)
            .then(|| format!("found-{rfc822_message_id}")))
    }

    /// Gmail's `drafts.create` of `raw`.
    pub(crate) async fn create_raw(
        &self,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<DraftIds, ProviderError> {
        self.enter(Call::DraftCreate, None)?;
        let delay = self.lock().action_delay;
        tokio::time::sleep(delay).await;
        create(&mut self.lock(), raw, thread_id)
    }

    /// Gmail's `drafts.update` of `draft_id` with `raw`.
    pub(crate) async fn update_raw(
        &self,
        draft_id: &str,
        raw: &[u8],
        thread_id: Option<&str>,
    ) -> Result<DraftIds, ProviderError> {
        self.enter(Call::DraftUpdate, None)?;
        let delay = self.lock().action_delay;
        tokio::time::sleep(delay).await;
        update(&mut self.lock(), draft_id, raw, thread_id)
    }

    /// The trait's `send`, as Gmail's adapter makes it; a Graph-model send consumes its draft.
    pub(super) async fn send_mail(
        &self,
        mail: &OutgoingMail,
    ) -> Result<SentReceipt, ProviderError> {
        let raw = build_message(&mail.outgoing)?;
        let thread = mail.thread.as_deref();
        let receipt = match self.send_raw(&raw, thread).await {
            Err(ProviderError::NotFound) if thread.is_some() => self.send_raw(&raw, None).await,
            other => other,
        }?;
        let mut m = self.lock();
        m.sent_mail.push(mail.clone());
        if m.graph_model
            && let Some(remote) = &mail.remote
        {
            m.drafts.remove(&remote.draft_id);
        }
        Ok(receipt)
    }

    /// The trait's `draft_save`, as Gmail's adapter makes it.
    pub(super) async fn save_mail(&self, mail: &OutgoingMail) -> Result<DraftIds, ProviderError> {
        let raw = build_message(&mail.outgoing)?;
        let thread = mail.thread.as_deref();
        if let Some(remote) = &mail.remote {
            match self.update_raw(&remote.draft_id, &raw, thread).await {
                Err(ProviderError::NotFound) => {}
                other => return other,
            }
        }
        match self.create_raw(&raw, thread).await {
            Err(ProviderError::NotFound) if thread.is_some() => self.create_raw(&raw, None).await,
            other => other,
        }
    }
}

/// The `Message-ID` of raw bytes (brackets kept), by a line scan of the header block.
fn header_message_id(raw: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(raw);
    for line in text.lines() {
        if line.is_empty() {
            return None;
        }
        if let (Some(name), Some(value)) = (line.get(..11), line.get(11..))
            && name.eq_ignore_ascii_case("message-id:")
        {
            return Some(value.trim().to_string());
        }
    }
    None
}
