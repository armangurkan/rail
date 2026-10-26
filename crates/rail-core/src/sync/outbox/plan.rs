//! What one row asks of the provider: the threads the store still holds, and the `Change`
//! with its folders mapped to their remote ids; or, for a draft row, which path takes it.

use crate::ops::{Action, DraftId};
use crate::provider::{Change, RemoteId, ThreadTarget};
use crate::store::outbox::OutboxRow;
use crate::store::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Plan {
    /// No thread of the row is left: nothing to send.
    Nothing,
    Send {
        threads: Vec<ThreadTarget>,
        change: Change,
    },
    /// A label or folder the action names is no longer in the store (deleted at the provider).
    LabelGone,
    /// A `Send` row: the worker's send path builds and sends the draft.
    SendDraft(DraftId),
    /// A `PushDraft` row: create or update the draft's Gmail copy.
    PushDraft(DraftId),
    /// A `DeleteDraft` row: delete this Gmail draft.
    DeleteDraft(String),
}

fn labels(store: &Store, account: i64, ids: &[i64]) -> Result<Option<Vec<RemoteId>>, StoreError> {
    let mut remotes = Vec::new();
    for &id in ids {
        match store.folder_remote(account, id)? {
            Some(remote) => remotes.push(remote),
            None => return Ok(None),
        }
    }
    Ok(Some(remotes))
}

/// The provider change of a thread action; `None` when a folder it names is gone. Draft
/// actions were planned by their draft and never reach here.
fn change_for(store: &Store, account: i64, action: &Action) -> Result<Option<Change>, StoreError> {
    let change = match action {
        Action::Archive {} => Change::Archive,
        Action::Trash {} => Change::Trash,
        Action::Star { on } => Change::Star { on: *on },
        Action::Read { on } => Change::Read { on: *on },
        Action::Labels { add, remove } => {
            match (
                labels(store, account, add)?,
                labels(store, account, remove)?,
            ) {
                (Some(add), Some(remove)) => Change::Labels { add, remove },
                _ => return Ok(None),
            }
        }
        Action::Move { to } => match store.folder_remote(account, *to)? {
            Some(to) => Change::Move { to },
            None => return Ok(None),
        },
        Action::Unsnooze { messages } => Change::Unsnooze {
            messages: messages.clone(),
        },
        Action::Send { .. } | Action::PushDraft { .. } | Action::DeleteDraft { .. } => {
            return Ok(None);
        }
    };
    Ok(Some(change))
}

pub(super) fn plan(store: &Store, row: &OutboxRow) -> Result<Plan, StoreError> {
    // A draft row names no threads: it is planned by its draft.
    match &row.action {
        Action::Send { draft } => return Ok(Plan::SendDraft(*draft)),
        Action::PushDraft { draft } => return Ok(Plan::PushDraft(*draft)),
        Action::DeleteDraft {
            remote_draft_id, ..
        } => return Ok(Plan::DeleteDraft(remote_draft_id.clone())),
        _ => {}
    }
    // The store already shows this row's change: the provider sees each message as it was.
    let threads = store.row_targets(row)?;
    if threads.is_empty() {
        return Ok(Plan::Nothing);
    }
    let Some(action) = store.row_action(row)? else {
        return Ok(Plan::LabelGone);
    };
    Ok(match change_for(store, row.account_id, &action)? {
        Some(change) => Plan::Send { threads, change },
        None => Plan::LabelGone,
    })
}
