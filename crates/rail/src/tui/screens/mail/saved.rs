//! Saving the filter in force under a name: `S` (or `:save-search`) opens the one-line name
//! prompt, `Enter` sends `saved_search.put`, and the answer — the whole new list — replaces
//! what the screen holds and rebuilds the sidebar.
//!
//! The prompt refuses a clashing or malformed name itself, through the same
//! `config::saved::check_put` the daemon uses, so the wording cannot drift and nothing
//! reaches the daemon that it would only refuse.

use rail_core::config::SavedSearch;
use rail_core::config::saved::check_put;
use rail_core::ops::OpError;

use super::{MailScreen, error_line};
use crate::tui::app::Effect;
use crate::tui::modals::prompt::{Check, Make};
use crate::tui::modals::{Command, Modal, TextPrompt};

/// What `S` names the filter in force.
const SAVE_TITLE: &str = "Save this filter as a search";
/// The prompt's label, before the typed name.
const NAME_LABEL: &str = "Name: ";

impl MailScreen {
    /// `S` and `:save-search`: the empty name prompt over the list, or FR-2's notice when
    /// there is no filter to name.
    pub(super) fn save_search_prompt(&mut self) -> Vec<Effect> {
        if self.filter.is_empty() {
            self.status
                .notify("Set a filter with / first, then save it.", self.tick);
            return Vec::new();
        }
        let check = self.name_check(self.filter_text.clone(), None);
        let make: Make = Box::new(|name| Command::SaveSearch { name });
        self.modal = Some(Modal::Prompt(TextPrompt::new(
            SAVE_TITLE, NAME_LABEL, "", check, make,
        )));
        Vec::new()
    }

    /// The prompt's own validator: FR-4, FR-5 and FR-6 against the list the screen holds,
    /// in §8's wording, so a refusal reads the same whether the prompt or the daemon made it.
    pub(super) fn name_check(&self, query: String, replaces: Option<String>) -> Check {
        let saved = self.saved.clone();
        Box::new(move |name: &str| check_put(&saved, name, &query, replaces.as_deref()))
    }

    /// Save the filter in force as `name`.
    pub(super) fn save_search(&mut self, name: String) -> Vec<Effect> {
        let query = self.filter_text.clone();
        vec![self.ctx.link.saved_search_put(name, query, None)]
    }

    /// `saved_search.put` or `.remove` answered. On `Ok` the whole new list replaces what the
    /// screen holds and the sidebar is read again, so its rows and its positional counts are
    /// rebuilt together; on `Err` the list is left as it was and the reason is shown.
    pub(super) fn saved_searches_changed(
        &mut self,
        result: Result<Vec<SavedSearch>, OpError>,
    ) -> Vec<Effect> {
        match result {
            Ok(searches) => {
                self.saved = searches;
                vec![self.read_sidebar()]
            }
            Err(error) => {
                tracing::warn!(code = ?error.code, "a saved-search write failed");
                let text = format!(
                    "Could not change the saved searches: {}",
                    error_line(&error)
                );
                self.status.error(text, self.ctx.clock.now(), self.tick);
                Vec::new()
            }
        }
    }
}
