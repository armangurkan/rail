//! `:search-counts`: show or hide the unread count beside each saved search, for the session
//! and in `config.toml`.
//!
//! Off, the screen passes no saved filters to either sidebar read, so `Store::saved_counts`
//! runs no query at all — the one thing saved searches cost on a large mailbox. The sidebar is
//! read again at once, because the counts on screen come from the last read: without it the
//! rows would keep drawing counts that are now off.

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;

impl MailScreen {
    /// Flip the saved-search counts, say so, and save: `saved_search_counts = false` when
    /// off; on removes the key, since on is the default.
    pub(super) fn toggle_search_counts(&mut self) -> Vec<Effect> {
        self.search_counts = !self.search_counts;
        let (notice, value) = if self.search_counts {
            ("saved-search counts: on", None)
        } else {
            ("saved-search counts: off", Some(false))
        };
        let mut effects = self.flip_and_save(
            Some(notice.to_string()),
            "saved_search_counts",
            Saved::Bool(value),
            |result| MailEvent::SearchCountsSaved { result },
        );
        effects.push(self.read_sidebar());
        effects
    }
}
