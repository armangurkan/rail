//! Bulk selection: `* a` selects every thread the mailbox and filter match, `* f` everything
//! from one sender, `v` a range; the state those keys keep between presses, and the question a
//! large archive, star or mark-read asks before it is sent.

use rail_core::ops::{Action as MailAction, MAX_UNDO_ROWS, RowToken};

use super::header::thousands;
use super::selection::Picked;
use super::{MailScreen, Modal};
use crate::tui::app::Effect;
use crate::tui::modals::{Command, Confirm};
use crate::tui::text::{escape, fit};

/// Characters of the one trashed thread's subject the question shows.
const SHOWN_SUBJECT_CHARS: usize = 120;

/// Threads one bulk select takes at most: ~50 `action.apply` chunks and ~1 undo chunk per
/// account, about 300 KB of ids and flags.
#[cfg(test)]
pub(crate) const MAX_SELECTION: usize = 10_000;

/// What the three bulk keys keep between presses.
#[derive(Debug, Default)]
pub(crate) struct BulkState {
    /// `v`: the thread the range anchors on, while range mode is on.
    anchor: Option<i64>,
    /// The last select-all was capped: the header says `(more match)`.
    pub(super) more: bool,
    /// The newest `* a` read's sequence number: an older answer is dropped.
    #[cfg(test)]
    pub(super) latest_match: u64,
}

impl BulkState {
    /// Range mode is on.
    pub(super) fn range_on(&self) -> bool {
        self.anchor.is_some()
    }
}

/// The confirmation's box title for `action`. Fixed words: no mail content reaches a question.
pub(super) fn title(action: &MailAction) -> &'static str {
    match action {
        MailAction::Archive {} => "Archive?",
        MailAction::Trash {} => "Trash?",
        MailAction::Star { on: true } => "Star?",
        MailAction::Star { on: false } => "Unstar?",
        MailAction::Read { on: true } => "Mark read?",
        MailAction::Read { on: false } => "Mark unread?",
        MailAction::Labels { .. } => "Change the labels?",
        MailAction::Move { .. } => "Move?",
        MailAction::Send { .. } => "Send?",
        MailAction::PushDraft { .. } => "Save the draft?",
        MailAction::DeleteDraft { .. } => "Discard the draft?",
    }
}

/// The confirmation's question for `action` over `count` threads, counted with commas. Fixed
/// words and a number only, so no mail content can reach it.
pub(super) fn question(action: &MailAction, count: usize) -> String {
    let n = thousands(u64::try_from(count).unwrap_or(u64::MAX));
    match action {
        MailAction::Archive {} => format!("Archive {n} threads?"),
        MailAction::Trash {} => format!("Trash {n} threads?"),
        MailAction::Star { on: true } => format!("Star {n} threads?"),
        MailAction::Star { on: false } => format!("Unstar {n} threads?"),
        MailAction::Read { on: true } => format!("Mark {n} threads read?"),
        MailAction::Read { on: false } => format!("Mark {n} threads unread?"),
        MailAction::Labels { .. } => format!("Change the labels of {n} threads?"),
        MailAction::Move { .. } => format!("Move {n} threads?"),
        MailAction::Send { .. } => format!("Send {n} messages?"),
        MailAction::PushDraft { .. } => format!("Save {n} drafts?"),
        MailAction::DeleteDraft { .. } => format!("Discard {n} drafts?"),
    }
}

/// `action` over `count` threads asks first: more than `threshold` threads, which `0` turns
/// off. Only `e`, `s` and `u` ask this way — trash asks at every size through its own question,
/// and the label and move pickers' own `Enter` is their confirmation.
pub(super) fn needs_confirm(action: &MailAction, count: usize, threshold: u32) -> bool {
    let asks = matches!(
        action,
        MailAction::Archive {} | MailAction::Star { .. } | MailAction::Read { .. }
    );
    asks && threshold > 0 && u64::try_from(count).unwrap_or(u64::MAX) > u64::from(threshold)
}

/// `rows` split into the batches one `action.undo` takes. A bulk action enqueues one outbox
/// row per `action.apply` call — about 58 at `MAX_SELECTION` — so this is defensive; the
/// request's own bound is `MAX_UNDO_ROWS`, and one `z` must take the whole set back.
pub(super) fn undo_batches(rows: Vec<RowToken>) -> Vec<Vec<RowToken>> {
    rows.chunks(MAX_UNDO_ROWS)
        .map(<[RowToken]>::to_vec)
        .collect()
}

impl MailScreen {
    /// `d`: the trash question, which asks at every size. One thread names its subject;
    /// several are counted, and each provider's note follows.
    pub(super) fn confirm_trash(&mut self, targets: Vec<(i64, Picked)>) -> Vec<Effect> {
        let first = targets.first().map(|(id, _)| *id);
        let subject = self
            .list
            .rows()
            .iter()
            .find(|r| Some(r.key.thread_id) == first)
            .map(|r| r.subject.clone())
            .or_else(|| self.reader.as_ref().map(|r| r.detail().subject.clone()));
        let line = match (targets.len(), subject) {
            (1, Some(subject)) => {
                format!("Trash \"{}\"?", fit(&escape(&subject), SHOWN_SUBJECT_CHARS))
            }
            (n, _) => super::bulk::question(&MailAction::Trash {}, n),
        };
        let mut lines = vec![line];
        lines.extend(self.trash_notes(&targets));
        self.actions.confirming = Some((MailAction::Trash {}, targets));
        self.modal = Some(Modal::Confirm(Confirm::new(
            "Trash?",
            lines,
            Command::Trash,
        )));
        Vec::new()
    }

    /// Ask before applying `action` to this many threads: the fixed question and count of
    /// `bulk`, over the list.
    pub(super) fn confirm_action(
        &mut self,
        action: MailAction,
        targets: Vec<(i64, Picked)>,
    ) -> Vec<Effect> {
        let lines = vec![super::bulk::question(&action, targets.len())];
        let head = super::bulk::title(&action);
        self.actions.confirming = Some((action, targets));
        self.modal = Some(Modal::Confirm(Confirm::new(head, lines, Command::Trash)));
        Vec::new()
    }

    /// `Esc` in the list: end range mode, keeping the range; else clear the selection
    /// (the filter line, when open, took the key before this).
    pub(super) fn clear_selection_or_range(&mut self) -> Vec<crate::tui::app::Effect> {
        if self.bulk.range_on() {
            self.bulk.anchor = None;
            return Vec::new();
        }
        self.actions.selection.clear();
        Vec::new()
    }
}

#[cfg(test)]
impl MailScreen {
    /// Range mode is on: `v` anchored it and neither `v` nor `Esc` has ended it.
    pub(crate) fn bulk_range_on(&self) -> bool {
        self.bulk.range_on()
    }
}
