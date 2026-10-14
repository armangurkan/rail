//! `:thread-order`: run the reader's older messages oldest first or newest first for the
//! session, and save the choice.

use rail_core::config::ThreadOrder;

use super::MailScreen;
use super::themes::Saved;
use crate::tui::app::Effect;
use crate::tui::data::MailEvent;

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
        let notice = format!("thread order: {}", order.as_str());
        self.flip_and_save(Some(notice), "thread_order", Saved::Text(value), |result| {
            MailEvent::ThreadOrderSaved { result }
        })
    }
}
