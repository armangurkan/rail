//! `apply_local` for a one-folder mailbox (`FolderModel::SingleFolder`, Microsoft 365): the
//! scope rule (`actions::scope`) picks the messages from the store's facts; archive, trash and
//! move put a picked message in the target folder alone, and star and read set the flag on
//! picked messages only. Each change is recorded per message in `Prior`'s shape, so undo,
//! `restack` and `reapply_live` work unchanged.

use rusqlite::Transaction;

use super::StoreError;
use super::apply_local::{
    Prior, add_membership, in_folder, remove_membership, set_flag, system_folder,
};
use super::targets::{folder_remote_in, folders_of, targets_in};
use crate::actions::scope::touched;
use crate::ops::Action;
use crate::provider::Change;

/// `action` as the change the scope rule reads, with the folder a move-like change puts
/// messages in. `None`: nothing to do here (labels are refused before they are queued, a
/// draft action names no threads, or the target folder is gone).
fn planned(
    tx: &Transaction,
    account: i64,
    action: &Action,
) -> Result<Option<(Change, Option<i64>)>, StoreError> {
    Ok(match action {
        Action::Archive {} => {
            system_folder(tx, account, "archive")?.map(|f| (Change::Archive, Some(f)))
        }
        Action::Trash {} => system_folder(tx, account, "trash")?.map(|f| (Change::Trash, Some(f))),
        Action::Move { to } => folder_remote_in(tx, account, *to)?
            .map(|remote| (Change::Move { to: remote }, Some(*to))),
        Action::Star { on } => Some((Change::Star { on: *on }, None)),
        Action::Read { on } => Some((Change::Read { on: *on }, None)),
        Action::Unsnooze { messages } => system_folder(tx, account, "inbox")?.map(|f| {
            let change = Change::Unsnooze {
                messages: messages.clone(),
            };
            (change, Some(f))
        }),
        Action::Labels { .. }
        | Action::Send { .. }
        | Action::PushDraft { .. }
        | Action::DeleteDraft { .. } => None,
    })
}

/// Put the message in `folder` alone: every other folder it is in comes off.
fn replace_folder(tx: &Transaction, prior: &mut Prior, folder: i64) -> Result<(), StoreError> {
    for old in folders_of(tx, prior.message)? {
        if old != folder {
            remove_membership(tx, prior.message, prior.thread, old)?;
            prior.removed.push(old);
        }
    }
    if !in_folder(tx, prior.message, folder)? {
        add_membership(tx, prior.message, prior.thread, folder)?;
        prior.added.push(folder);
    }
    Ok(())
}

/// Apply `action` to the messages of `threads` the scope rule picks; what it changed.
pub(super) fn apply(
    tx: &Transaction,
    account: i64,
    action: &Action,
    threads: &[i64],
) -> Result<Vec<Prior>, StoreError> {
    let Some((change, folder)) = planned(tx, account, action)? else {
        return Ok(Vec::new());
    };
    let mut priors = Vec::new();
    for stored in targets_in(tx, account, threads)? {
        for index in touched(&change, &stored.target.messages) {
            let facts = &stored.target.messages[index];
            let mut prior = Prior {
                message: stored.messages[index],
                thread: stored.thread,
                ..Prior::default()
            };
            match (&change, folder) {
                (Change::Read { on }, _) if facts.unread == *on => {
                    set_flag(tx, "is_read", prior.message, prior.thread, *on)?;
                    prior.read = Some(!facts.unread);
                }
                (Change::Star { on }, _) if facts.starred != *on => {
                    set_flag(tx, "is_starred", prior.message, prior.thread, *on)?;
                    prior.starred = Some(facts.starred);
                }
                (
                    Change::Archive | Change::Trash | Change::Move { .. } | Change::Unsnooze { .. },
                    Some(folder),
                ) => {
                    replace_folder(tx, &mut prior, folder)?;
                }
                _ => {}
            }
            if prior.changed() {
                priors.push(prior);
            }
        }
    }
    Ok(priors)
}
