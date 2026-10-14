//! `:time`: switch between 24-hour and 12-hour (am/pm) times for the session and save the
//! choice.

use rail_core::config::TimeFormat;

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;

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
        let notice = format!("time: {}", format.as_str());
        self.flip_and_save(Some(notice), "time", Saved::Text(value), |result| {
            MailEvent::TimeSaved { result }
        })
    }
}
