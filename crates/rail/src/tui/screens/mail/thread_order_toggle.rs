//! `:thread-order`: run the reader's older messages oldest first or newest first for the
//! session, and save the choice.

use rail_core::config::ThreadOrder;

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;
use crate::tui::text::escape;

impl MailScreen {
    /// Flip the order (the open reader's too), say so, and save: `thread_order = "newest"`;
    /// oldest first removes the key, since it is the default.
    pub(super) fn toggle_thread_order(&mut self) -> Vec<Effect> {
        let order = match self.ctx.thread_order {
            ThreadOrder::Oldest => ThreadOrder::Newest,
            ThreadOrder::Newest => ThreadOrder::Oldest,
        };
        self.ctx.thread_order = order;
        if let Some(reader) = &mut self.reader {
            reader.set_order(order);
        }
        let value = match order {
            ThreadOrder::Oldest => None,
            ThreadOrder::Newest => Some(order.as_str().to_string()),
        };
        self.status
            .notify(format!("thread order: {}", order.as_str()), self.tick);
        vec![
            self.save_general("thread_order", Saved::Text(value), |result| {
                MailEvent::ThreadOrderSaved { result }
            }),
        ]
    }

    /// The config write answered: a failure keeps the order for the session and says so.
    pub(super) fn thread_order_saved(&mut self, result: Result<(), String>) -> Vec<Effect> {
        if let Err(reason) = result {
            tracing::warn!("could not save the thread order to config.toml");
            let text = format!("Could not save the thread order: {}", escape(&reason));
            self.status.error(text, self.ctx.clock.now(), self.tick);
        }
        Vec::new()
    }
}
