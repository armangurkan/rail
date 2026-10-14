//! `:time`: switch between 24-hour and 12-hour (am/pm) times for the session and save the
//! choice.

use rail_core::config::TimeFormat;

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;
use crate::tui::text::escape;

impl MailScreen {
    /// Flip the format on the clock (the open reader's copy too), say so, and save:
    /// `time = "12h"` for 12-hour; 24-hour removes the key, since it is the default.
    pub(super) fn toggle_time(&mut self) -> Vec<Effect> {
        let format = match self.ctx.clock.format() {
            TimeFormat::H24 => TimeFormat::H12,
            TimeFormat::H12 => TimeFormat::H24,
        };
        self.ctx.clock.set_format(format);
        if let Some(reader) = &mut self.reader {
            reader.set_clock(self.ctx.clock.clone());
        }
        let value = match format {
            TimeFormat::H24 => None,
            TimeFormat::H12 => Some(format.as_str().to_string()),
        };
        self.status
            .notify(format!("time: {}", format.as_str()), self.tick);
        vec![
            self.save_general("time", Saved::Text(value), |result| MailEvent::TimeSaved {
                result,
            }),
        ]
    }

    /// The config write answered: a failure keeps the setting for the session and says so.
    pub(super) fn time_saved(&mut self, result: Result<(), String>) -> Vec<Effect> {
        if let Err(reason) = result {
            tracing::warn!("could not save the time setting to config.toml");
            let text = format!("Could not save the time setting: {}", escape(&reason));
            self.status.error(text, self.ctx.clock.now(), self.tick);
        }
        Vec::new()
    }
}
