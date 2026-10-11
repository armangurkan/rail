//! What one row asks of the provider, in its ids: the threads the store still holds, and the
//! labels to add and remove (system labels for archive, read and star; folders mapped to
//! their remote ids).

use crate::ops::Action;
use crate::provider::{INBOX_LABEL, RemoteId, STARRED_LABEL, UNREAD_LABEL};
use crate::store::outbox::OutboxRow;
use crate::store::{Store, StoreError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Remote {
    Trash,
    Modify {
        add: Vec<RemoteId>,
        remove: Vec<RemoteId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Plan {
    /// No thread of the row is left: nothing to send.
    Nothing,
    Send {
        threads: Vec<RemoteId>,
        change: Remote,
    },
    /// A label the action names is no longer in the store (deleted in Gmail).
    LabelGone,
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

pub(super) fn plan(store: &Store, row: &OutboxRow) -> Result<Plan, StoreError> {
    let threads = store.thread_remotes(row.account_id, &row.thread_ids)?;
    if threads.is_empty() {
        return Ok(Plan::Nothing);
    }
    let system = |s: &str| vec![s.to_string()];
    let (add, remove) = match &row.action {
        Action::Trash {} => {
            return Ok(Plan::Send {
                threads,
                change: Remote::Trash,
            });
        }
        Action::Archive {} => (Vec::new(), system(INBOX_LABEL)),
        Action::Star { on: true } => (system(STARRED_LABEL), Vec::new()),
        Action::Star { on: false } => (Vec::new(), system(STARRED_LABEL)),
        Action::Read { on: true } => (Vec::new(), system(UNREAD_LABEL)),
        Action::Read { on: false } => (system(UNREAD_LABEL), Vec::new()),
        Action::Labels { add, remove } => {
            let (Some(add), Some(remove)) = (
                labels(store, row.account_id, add)?,
                labels(store, row.account_id, remove)?,
            ) else {
                return Ok(Plan::LabelGone);
            };
            (add, remove)
        }
        Action::Move { to } => match labels(store, row.account_id, &[*to])? {
            Some(add) => (add, system(INBOX_LABEL)),
            None => return Ok(Plan::LabelGone),
        },
    };
    Ok(Plan::Send {
        threads,
        change: Remote::Modify { add, remove },
    })
}
