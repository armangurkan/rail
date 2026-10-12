//! The mock's Drafts: a map of draft id to message, served to `drafts.*` calls.

use super::{Mailbox, MockDraft};
use crate::provider::{DraftIds, ProviderError, RemoteId};

pub(super) fn create(m: &mut Mailbox, raw: &[u8], thread: Option<&str>) -> DraftIds {
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
    ids
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
