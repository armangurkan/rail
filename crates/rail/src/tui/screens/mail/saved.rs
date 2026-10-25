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

use super::sidebar_tree::RowKind;
use super::{MailScreen, error_line};
use crate::tui::app::Effect;
use crate::tui::data::ScopeChoice;
use crate::tui::modals::prompt::{Check, Make};
use crate::tui::modals::{Command, Confirm, Modal, TextPrompt};
use crate::tui::text::escape;

/// What `S` names the filter in force.
const SAVE_TITLE: &str = "Save this filter as a search";
/// What `r` renames.
const RENAME_TITLE: &str = "Rename this saved search";
/// What `d` asks before removing.
const REMOVE_TITLE: &str = "Remove a saved search";
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

    /// The saved search under the sidebar cursor, by position. On any other row this is
    /// FR-16's notice and `None`: `r` and `d` never do nothing silently.
    fn cursor_search(&mut self) -> Option<usize> {
        if let Some(RowKind::Saved { index }) = self.sidebar.selected().map(|r| r.kind.clone()) {
            return Some(index);
        }
        self.status.notify("That is not a saved search.", self.tick);
        None
    }

    /// `r` and `:rename-search`: the name prompt over the row, pre-filled with its name. The
    /// query is not shown and does not change.
    pub(super) fn rename_search_prompt(&mut self, index: usize) -> Vec<Effect> {
        let Some(entry) = self.saved.get(index) else {
            return Vec::new();
        };
        let (name, query) = (entry.name.clone(), entry.query.clone());
        // FR-4's carve-out: the entry being renamed is skipped by the duplicate test, so the
        // same name, or the same name in another case, is accepted.
        let check = self.name_check(query, Some(name.clone()));
        let make: Make = Box::new(move |name| Command::RenameSearch { index, name });
        self.modal = Some(Modal::Prompt(TextPrompt::new(
            RENAME_TITLE,
            NAME_LABEL,
            &name,
            check,
            make,
        )));
        Vec::new()
    }

    /// `d` and `:remove-search`: the confirmation, which says the mail is not touched.
    pub(super) fn remove_search_confirm(&mut self, index: usize) -> Vec<Effect> {
        let Some(entry) = self.saved.get(index) else {
            return Vec::new();
        };
        let line = format!(
            "Remove the saved search \"{}\"? The mail it finds is not touched.",
            escape(&entry.name)
        );
        self.modal = Some(Modal::Confirm(Confirm::new(
            REMOVE_TITLE,
            vec![line],
            Command::RemoveSearch { index },
        )));
        Vec::new()
    }

    /// `r` on the sidebar cursor's row.
    pub(super) fn rename_cursor_search(&mut self) -> Vec<Effect> {
        match self.cursor_search() {
            Some(index) => self.rename_search_prompt(index),
            None => Vec::new(),
        }
    }

    /// `d` on the sidebar cursor's row.
    pub(super) fn remove_cursor_search(&mut self) -> Vec<Effect> {
        match self.cursor_search() {
            Some(index) => self.remove_search_confirm(index),
            None => Vec::new(),
        }
    }

    /// Rename the search at `index` to `name`, keeping its query. The rename is remembered
    /// until the answer arrives, so the open scope and the sidebar cursor can follow the row
    /// instead of reading a changed name as a removal.
    pub(super) fn rename_search(&mut self, index: usize, name: String) -> Vec<Effect> {
        let Some(entry) = self.saved.get(index) else {
            return Vec::new();
        };
        let (old, query) = (entry.name.clone(), entry.query.clone());
        self.pending_rename = Some((old.clone(), name.clone()));
        vec![self.ctx.link.saved_search_put(name, query, Some(old))]
    }

    /// Remove the search at `index`.
    pub(super) fn remove_search(&mut self, index: usize) -> Vec<Effect> {
        let Some(entry) = self.saved.get(index) else {
            return Vec::new();
        };
        vec![self.ctx.link.saved_search_remove(entry.name.clone())]
    }

    /// `saved_search.put` or `.remove` answered. On `Ok`, in FR-17's order: a rename in
    /// flight re-keys the open scope and the sidebar cursor, so the row is followed rather
    /// than read as a removal; the whole new list replaces what the screen holds; the open
    /// search is found again by name; the sidebar is read again, so its rows and its
    /// positional counts are rebuilt together; and the list is read again when the scope
    /// changed. On `Err` the list is left as it was and the reason is shown.
    pub(super) fn saved_searches_changed(
        &mut self,
        result: Result<Vec<SavedSearch>, OpError>,
    ) -> Vec<Effect> {
        let renamed = self.pending_rename.take();
        match result {
            Ok(searches) => {
                if let Some((old, new)) = renamed {
                    self.follow_rename(&old, &new);
                }
                self.saved = searches;
                let mut effects = vec![self.read_sidebar()];
                effects.extend(self.resolve_open_search());
                effects
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

    /// A rename carries the row's identity with it: the scope's name and the cursor's key are
    /// both the old name, and a by-name lookup after the list is replaced would find neither.
    fn follow_rename(&mut self, old: &str, new: &str) {
        if let ScopeChoice::Saved { name, .. } = &mut self.scope
            && name.eq_ignore_ascii_case(old)
        {
            *name = new.to_string();
        }
        self.sidebar.rename_cursor(old, new);
    }

    /// FR-17 step 3: the open saved search, found again by name in the new list. A changed
    /// position is followed with a fresh list read; a name that is gone falls back to All
    /// inboxes and says so.
    fn resolve_open_search(&mut self) -> Vec<Effect> {
        let ScopeChoice::Saved { index, name } = &self.scope else {
            return Vec::new();
        };
        let (index, name) = (*index, name.clone());
        match self.saved.iter().position(|s| s.name == name) {
            Some(at) if at == index => Vec::new(),
            Some(at) => self.set_scope(ScopeChoice::Saved { index: at, name }),
            None => {
                self.status.notify(
                    "The saved search you had open is gone; showing All inboxes.",
                    self.tick,
                );
                self.set_scope(ScopeChoice::Unified)
            }
        }
    }
}
