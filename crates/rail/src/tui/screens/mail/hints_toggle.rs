//! `:hints`: turn the key hints off or on for the session and save the choice.

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;

impl MailScreen {
    /// Flip the hints and save: `hints = false` when off (`hints: off`); on removes the key,
    /// since on is the default, and says nothing: the hints coming back are the confirmation,
    /// so a `hints: off` still showing goes too.
    pub(super) fn toggle_hints(&mut self) -> Vec<Effect> {
        self.hints = !self.hints;
        let (notice, value) = if self.hints {
            self.status.dismiss("hints: off");
            (None, None)
        } else {
            (Some("hints: off".to_string()), Some(false))
        };
        self.flip_and_save(notice, "hints", Saved::Bool(value), |result| {
            MailEvent::HintsSaved { result }
        })
    }
}
