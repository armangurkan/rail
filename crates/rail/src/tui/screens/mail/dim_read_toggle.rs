//! `:dim-read`: cycle read-mail dimming and save the choice.

use rail_core::config::DimRead;

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;

impl MailScreen {
    pub(super) fn cycle_dim_read(&mut self) -> Vec<Effect> {
        self.ctx.dim_read = match self.ctx.dim_read {
            DimRead::Dot => DimRead::Subject,
            DimRead::Subject => DimRead::All,
            DimRead::All => DimRead::Dot,
        };
        let mode = self.ctx.dim_read;
        let value = (mode != DimRead::Dot).then(|| mode.as_str().to_string());
        self.flip_and_save(
            Some(format!("dim read: {}", mode.as_str())),
            "dim_read",
            Saved::Text(value),
            |result| MailEvent::DimReadSaved { result },
        )
    }
}
