//! `:hints`: turn the key hints off or on for the session and save the choice.

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;
use crate::tui::text::escape;

impl MailScreen {
    /// Flip the hints, say so, and save: `hints = false` when off; on removes the key, since
    /// on is the default.
    pub(super) fn toggle_hints(&mut self) -> Vec<Effect> {
        self.hints = !self.hints;
        let (notice, value) = if self.hints {
            ("hints: on", None)
        } else {
            ("hints: off", Some(false))
        };
        self.status.notify(notice, self.tick);
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
