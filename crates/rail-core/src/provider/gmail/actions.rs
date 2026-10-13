//! `threads.modify` and `threads.trash`: one POST per thread, and the table that turns a
//! `Change` into them (labels for archive, read, star and move; trash has its own call).
//! Thread and label ids are checked like every id Rail puts in a path before anything is sent.

use serde_json::json;

use super::{COST_THREAD_MODIFY, COST_THREAD_TRASH, GmailProvider, MAX_LABELS_PER_MODIFY, safe_id};
use crate::provider::{
    Change, INBOX_LABEL, ProviderError, RemoteId, STARRED_LABEL, ThreadTarget, UNREAD_LABEL,
};

/// The longest thread or label id Rail puts in a path or a body.
const MAX_ACTION_ID_BYTES: usize = 128;

fn checked(ids: &[RemoteId], what: &str) -> Result<(), ProviderError> {
    if ids.iter().all(|id| safe_id(id, MAX_ACTION_ID_BYTES)) {
        Ok(())
    } else {
        Err(ProviderError::permanent(&format!(
            "a {what} id is not one Rail sends to Gmail"
        )))
    }
}

/// What a `Change` is in Gmail's calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum GmailCall {
    Trash,
    Modify {
        add: Vec<RemoteId>,
        remove: Vec<RemoteId>,
    },
}

/// Archive removes `INBOX`, read removes `UNREAD`, star adds `STARRED`, a move adds its label
/// and removes `INBOX`; labels go as given; trash is `threads.trash`.
pub(crate) fn gmail_call(change: &Change) -> GmailCall {
    let one = |label: &str| vec![label.to_string()];
    let (add, remove) = match change {
        Change::Trash => return GmailCall::Trash,
        Change::Archive => (Vec::new(), one(INBOX_LABEL)),
        Change::Star { on: true } => (one(STARRED_LABEL), Vec::new()),
        Change::Star { on: false } => (Vec::new(), one(STARRED_LABEL)),
        Change::Read { on: true } => (Vec::new(), one(UNREAD_LABEL)),
        Change::Read { on: false } => (one(UNREAD_LABEL), Vec::new()),
        Change::Labels { add, remove } => (add.clone(), remove.clone()),
        Change::Move { to } => (vec![to.clone()], one(INBOX_LABEL)),
    };
    GmailCall::Modify { add, remove }
}

impl GmailProvider {
    /// `change` on each thread, as today's calls.
    pub(crate) async fn apply_change(
        &self,
        threads: &[ThreadTarget],
        change: &Change,
    ) -> Result<(), ProviderError> {
        let ids = ThreadTarget::thread_ids(threads);
        match gmail_call(change) {
            GmailCall::Trash => self.trash(&ids).await,
            GmailCall::Modify { add, remove } => self.modify(&ids, &add, &remove).await,
        }
    }

    pub(crate) async fn modify(
        &self,
        ids: &[RemoteId],
        add: &[RemoteId],
        remove: &[RemoteId],
    ) -> Result<(), ProviderError> {
        checked(ids, "thread")?;
        checked(add, "label")?;
        checked(remove, "label")?;
        if add.len() > MAX_LABELS_PER_MODIFY || remove.len() > MAX_LABELS_PER_MODIFY {
            return Err(ProviderError::permanent(
                "Gmail takes at most 100 labels per change",
            ));
        }
        let body = json!({"addLabelIds": add, "removeLabelIds": remove});
        for id in ids {
            self.api
                .post_json(
                    &format!("threads/{id}/modify"),
                    Some(&body),
                    COST_THREAD_MODIFY,
                )
                .await?;
        }
        Ok(())
    }

    pub(crate) async fn trash(&self, ids: &[RemoteId]) -> Result<(), ProviderError> {
        checked(ids, "thread")?;
        for id in ids {
            self.api
                .post_json(&format!("threads/{id}/trash"), None, COST_THREAD_TRASH)
                .await?;
        }
        Ok(())
    }
}
