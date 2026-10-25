//! Bulk selection: `* a` selects every thread the mailbox and filter match, `* f` everything
//! from one sender, `v` a range; the state those keys keep between presses, and the question a
//! large archive, star or mark-read asks before it is sent.

use rail_core::ops::Action as MailAction;

use super::MailScreen;

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

/// The confirmation's box title for `action`.
pub(super) fn title(_action: &MailAction) -> &'static str {
    ""
}

/// The confirmation's question for `action` over `count` threads, counted with commas.
pub(super) fn question(_action: &MailAction, _count: usize) -> String {
    String::new()
}

/// `action` over `count` threads asks first: more than `threshold` threads, which `0` turns
/// off. Trash asks at every size, so it never reaches here.
pub(super) fn needs_confirm(_action: &MailAction, _count: usize, _threshold: u32) -> bool {
    false
}

impl MailScreen {
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
