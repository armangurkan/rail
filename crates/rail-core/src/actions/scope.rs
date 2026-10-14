//! The scope rule of a one-folder mailbox (`FolderModel::SingleFolder`): which messages of a
//! thread a change touches. The Graph adapter builds its requests from it and the store's
//! optimistic change (`apply_local`) decides what to change from it, so both agree on every
//! message. Archive, trash and move leave the user's own mail in Sent and Drafts where it is.

use crate::provider::{Change, FolderKind, TargetMessage};

/// The user's own mail: never archived, trashed or moved, and never the newest received.
const OWN_MAIL: [FolderKind; 2] = [FolderKind::Sent, FolderKind::Drafts];

/// What trash leaves where it is: the user's own mail, and what is in Deleted Items already.
const KEPT_BY_TRASH: [FolderKind; 3] = [FolderKind::Sent, FolderKind::Drafts, FolderKind::Trash];

/// The indices of `messages` that `change` touches, ascending. `Labels` never reaches a
/// one-folder mailbox (it is refused first) and touches nothing.
pub fn touched(change: &Change, messages: &[TargetMessage]) -> Vec<usize> {
    match change {
        Change::Archive => matching(messages, |m| m.folder == Some(FolderKind::Inbox)),
        Change::Trash => matching(messages, |m| !in_kinds(m, &KEPT_BY_TRASH)),
        Change::Move { to } => matching(messages, |m| {
            !in_kinds(m, &OWN_MAIL) && m.folder_remote.as_ref() != Some(to)
        }),
        Change::Read { on: true } => matching(messages, |m| m.unread),
        Change::Read { on: false } => newest(messages)
            .filter(|&i| !messages[i].unread)
            .into_iter()
            .collect(),
        Change::Star { on: true } if messages.iter().any(|m| m.starred) => Vec::new(),
        Change::Star { on: true } => newest(messages).into_iter().collect(),
        Change::Star { on: false } => matching(messages, |m| m.starred),
        Change::Labels { .. } => Vec::new(),
    }
}

fn matching(messages: &[TargetMessage], keep: impl Fn(&TargetMessage) -> bool) -> Vec<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| keep(m))
        .map(|(i, _)| i)
        .collect()
}

fn in_kinds(message: &TargetMessage, kinds: &[FolderKind]) -> bool {
    message.folder.is_some_and(|k| kinds.contains(&k))
}

/// The newest received message (not in Sent or Drafts): the largest `date`, a tie going to
/// the larger id. A thread holding only the user's own mail falls back to its newest message.
fn newest(messages: &[TargetMessage]) -> Option<usize> {
    latest(messages, |m| !in_kinds(m, &OWN_MAIL)).or_else(|| latest(messages, |_| true))
}

fn latest(messages: &[TargetMessage], keep: impl Fn(&TargetMessage) -> bool) -> Option<usize> {
    messages
        .iter()
        .enumerate()
        .filter(|(_, m)| keep(m))
        .max_by(|(_, a), (_, b)| (a.date, &a.id).cmp(&(b.date, &b.id)))
        .map(|(i, _)| i)
}
