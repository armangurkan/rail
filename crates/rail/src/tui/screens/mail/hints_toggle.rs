//! `:hints`: turn the key hints off or on for the session and save the choice.

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;
use crate::tui::text::escape;

impl MailScreen {
    /// Flip the hints and save: `hints = false` when off (`hints: off`); on removes the key,
    /// since on is the default, and says nothing: the hints coming back are the confirmation,
    /// so a `hints: off` still showing goes too.
    pub(super) fn toggle_hints(&mut self) -> Vec<Effect> {
        self.hints = !self.hints;
        let value = if self.hints {
            self.status.dismiss("hints: off");
            None
        } else {
            self.status.notify("hints: off", self.tick);
            Some(false)
        };
        vec![self.save_general("hints", Saved::Bool(value), |result| {
            MailEvent::HintsSaved { result }
        })]
    }

    /// The config write answered: a failure keeps the setting for the session and says so.
    pub(super) fn hints_saved(&mut self, result: Result<(), String>) -> Vec<Effect> {
        if let Err(reason) = result {
            tracing::warn!("could not save the hints setting to config.toml");
            let text = format!("Could not save the hints setting: {}", escape(&reason));
            self.status.error(text, self.ctx.clock.now(), self.tick);
        }
        Vec::new()
    }
}
