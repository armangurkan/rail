//! `:clock`: hide or show the header's clock for the session and save the choice.

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;
use crate::tui::text::escape;

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
        self.status.notify(notice, self.tick);
        vec![self.save_general("clock", Saved::Bool(value), |result| {
            MailEvent::ClockSaved { result }
        })]
    }

    /// The config write answered: a failure keeps the setting for the session and says so.
    pub(super) fn clock_saved(&mut self, result: Result<(), String>) -> Vec<Effect> {
        if let Err(reason) = result {
            tracing::warn!("could not save the clock setting to config.toml");
            let text = format!("Could not save the clock setting: {}", escape(&reason));
            self.status.error(text, self.ctx.clock.now(), self.tick);
        }
        Vec::new()
    }
}
