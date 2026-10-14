//! `:clock`: hide or show the header's clock for the session and save the choice.

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;

impl MailScreen {
    /// Flip the clock, say so, and save: `clock = false` when off; on removes the key, since
    /// on is the default.
    pub(super) fn toggle_clock(&mut self) -> Vec<Effect> {
        self.show_clock = !self.show_clock;
        let (notice, value) = if self.show_clock {
            ("clock: on", None)
        } else {
            ("clock: off", Some(false))
        };
        self.flip_and_save(
            Some(notice.to_string()),
            "clock",
            Saved::Bool(value),
            |result| MailEvent::ClockSaved { result },
        )
    }
}
