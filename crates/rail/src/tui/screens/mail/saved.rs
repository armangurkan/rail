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

use super::sidebar_tree::{RowKey, RowKind};
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
/// What a rename or a removal says when the row it named is no longer where it was.
const MOVED_NOTICE: &str = "The saved searches changed; try again.";
/// What opening a saved search says when its name is gone from the list. Named once: FR-17's
/// re-find and `resolve_saved_scope` both show it, and one of the two would otherwise drift.
const GONE_NOTICE: &str = "The saved search you had open is gone; showing All inboxes.";
/// FR-16, on a row that is not a saved search at all.
const NOT_A_SEARCH: &str = "That is not a saved search.";

impl MailScreen {
    /// There is filter text to name. The gate is the **text**, not the parsed filter: the
    /// text is what is saved, and the parsed filter lags it by the filter debounce, so `S`
    /// pressed straight after `Enter` on a line that plainly had text must not answer that
    /// there is no filter. Whitespace alone is no filter.
    pub(super) fn has_filter_text(&self) -> bool {
        !self.filter_text.trim().is_empty()
    }

    /// `S` and `:save-search`: the empty name prompt over the list, or FR-2's notice when
    /// there is no filter to name.
    pub(super) fn save_search_prompt(&mut self) -> Vec<Effect> {
        if !self.has_filter_text() {
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

    /// The saved search under the sidebar cursor, by position — and only while that position
    /// still holds the row's own search. The rows outlive the list they were built from (an
    /// earlier write's answer landing before the sidebar read comes back), so a position alone
    /// would hand `r` and `d` the next search along. On any other row this is FR-16's notice
    /// and `None`, on a row the list has outlived it is the moved notice: neither key ever
    /// does nothing silently, and neither ever acts on a search the row does not name.
    fn cursor_search(&mut self) -> Option<usize> {
        let row = self
            .sidebar
            .selected()
            .map(|r| (r.kind.clone(), r.key.clone()));
        let Some((RowKind::Saved { index }, RowKey::Saved(name))) = row else {
            self.status.notify(NOT_A_SEARCH, self.tick);
            return None;
        };
        if self.entry_at(index, &name).is_none() {
            self.say_searches_moved();
            return None;
        }
        Some(index)
    }

    /// `r` and `:rename-search`: the name prompt over the row, pre-filled with its name. The
    /// query is not shown and does not change.
    pub(super) fn rename_search_prompt(&mut self, index: usize) -> Vec<Effect> {
        let Some((name, query)) = self
            .saved
            .get(index)
            .map(|entry| (entry.name.clone(), entry.query.clone()))
        else {
            return Vec::new();
        };
        // FR-4's carve-out: the entry being renamed is skipped by the duplicate test, so the
        // same name, or the same name in another case, is accepted.
        let check = self.name_check(query, Some(name.clone()));
        // The name the prompt was opened on travels with the index, so `Enter` can only ever
        // rename the search the prompt named, however long the typing took.
        let old = name.clone();
        let make: Make = Box::new(move |typed| Command::RenameSearch {
            index,
            old: old.clone(),
            name: typed,
        });
        self.modal = Some(Modal::Prompt(TextPrompt::new(
            RENAME_TITLE,
            NAME_LABEL,
            &name,
            check,
            make,
        )));
        Vec::new()
    }

    /// `d` and `:remove-search`: the confirmation, which says the mail is not touched. The
    /// name it shows is the name it carries, so `y` cannot remove anything else.
    pub(super) fn remove_search_confirm(&mut self, index: usize) -> Vec<Effect> {
        let Some(name) = self.saved.get(index).map(|entry| entry.name.clone()) else {
            return Vec::new();
        };
        let line = format!(
            "Remove the saved search \"{}\"? The mail it finds is not touched.",
            escape(&name)
        );
        self.modal = Some(Modal::Confirm(Confirm::new(
            REMOVE_TITLE,
            vec![line],
            Command::RemoveSearch { index, name },
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

    /// The entry at `index`, but only while it is still the one called `name` — the identity
    /// the daemon keys on, compared the way the daemon compares it. `None` means the list was
    /// replaced under the open prompt or question (an earlier write's answer landing, FR-17),
    /// so the index no longer names what the user was shown.
    fn entry_at(&self, index: usize, name: &str) -> Option<&SavedSearch> {
        self.saved
            .get(index)
            .filter(|entry| entry.name.eq_ignore_ascii_case(name))
    }

    /// The entry a `Saved` scope names: the one at `index` while it still carries `name`, else
    /// the one called `name` wherever it has moved to, and `None` once nothing has that name.
    /// Every read derived from a saved scope goes through here, so a position whose list was
    /// replaced resolves to the search the user picked or to nothing — never to its neighbour.
    pub(super) fn saved_entry(&self, index: usize, name: &str) -> Option<&SavedSearch> {
        self.entry_at(index, name).or_else(|| {
            self.saved
                .iter()
                .find(|entry| entry.name.eq_ignore_ascii_case(name))
        })
    }

    /// A `Saved` scope re-keyed to where its search actually is, whoever asked to open it: a
    /// sidebar row, a palette entry, or FR-17's own re-find. Both are built from a list a
    /// write can replace before the sidebar read rebuilds them, and `store_scope` turns a
    /// `Saved` scope into every folder of every account, so a position that no longer names
    /// the search the user picked would open another search's mail — or, with no filter left
    /// to narrow it, the whole mailbox. A name that is gone falls back to All inboxes with
    /// the notice a removal already shows. Any other scope is returned untouched.
    pub(super) fn resolve_saved_scope(&mut self, scope: ScopeChoice) -> ScopeChoice {
        let ScopeChoice::Saved { index, name } = &scope else {
            return scope;
        };
        let found = self
            .saved
            .iter()
            .position(|entry| entry.name.eq_ignore_ascii_case(name));
        match found {
            Some(at) if at == *index => scope,
            Some(at) => ScopeChoice::Saved {
                index: at,
                name: name.clone(),
            },
            None => {
                self.status.notify(GONE_NOTICE, self.tick);
                ScopeChoice::Unified
            }
        }
    }

    /// The row moved under the open prompt, question or cursor: say so. The callers send
    /// nothing after it — never silently nothing, as FR-16 requires of `r` and `d`.
    fn say_searches_moved(&mut self) {
        self.status.notify(MOVED_NOTICE, self.tick);
    }

    /// `say_searches_moved` for the callers that answer with effects.
    fn searches_moved(&mut self) -> Vec<Effect> {
        self.say_searches_moved();
        Vec::new()
    }

    /// Rename the search the prompt named — `old`, at `index` when it opened — to `name`,
    /// keeping its query. The rename is remembered until the answer arrives, so the open
    /// scope and the sidebar cursor can follow the row instead of reading a changed name as a
    /// removal.
    pub(super) fn rename_search(&mut self, index: usize, old: &str, name: String) -> Vec<Effect> {
        let Some((old, query)) = self
            .entry_at(index, old)
            .map(|entry| (entry.name.clone(), entry.query.clone()))
        else {
            return self.searches_moved();
        };
        self.pending_rename = Some((old.clone(), name.clone()));
        vec![self.ctx.link.saved_search_put(name, query, Some(old))]
    }

    /// Remove the search the question named — `name`, at `index` when it was asked.
    pub(super) fn remove_search(&mut self, index: usize, name: &str) -> Vec<Effect> {
        let Some(found) = self.entry_at(index, name).map(|entry| entry.name.clone()) else {
            return self.searches_moved();
        };
        vec![self.ctx.link.saved_search_remove(found)]
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
                self.status.notify(GONE_NOTICE, self.tick);
                self.set_scope(ScopeChoice::Unified)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crossterm::event::KeyCode;
    use rail_core::paths::Paths;

    use super::*;
    use crate::tui::data::{FakeCall, FakeDaemon};
    use crate::tui::modals::ModalOutcome;
    use crate::tui::screens::mail::MailContext;
    use crate::tui::test_util::{key, mail_ctx, seed_store, settle, work_and_home};

    /// One saved search per name, each with a query of its own so a mix-up shows.
    fn searches(names: &[&str]) -> Vec<SavedSearch> {
        names
            .iter()
            .map(|n| SavedSearch {
                name: (*n).into(),
                query: format!("from:{n}@example.com"),
            })
            .collect()
    }

    /// A screen holding `names` as its saved searches, and the fake daemon behind it. The
    /// flows are driven directly, the way the palette's indexed targets drive them, so no
    /// list is ever read and `start()` is not needed.
    fn screen_with(names: &[&str]) -> (tempfile::TempDir, Arc<FakeDaemon>, MailScreen) {
        let saved = searches(names);
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(tmp.path());
        seed_store(
            &paths,
            &[("work", "w@example.com"), ("home", "h@example.com")],
            &[],
        );
        let fake = Arc::new(FakeDaemon::default());
        fake.saved.lock().unwrap().clone_from(&saved);
        let ctx = MailContext {
            saved_searches: saved,
            ..mail_ctx(&paths, fake.clone())
        };
        (tmp, fake, MailScreen::new(ctx, work_and_home()))
    }

    /// Every `SavedSearchPut` / `SavedSearchRemove` the fake daemon took, in order.
    fn saved_calls(fake: &FakeDaemon) -> Vec<FakeCall> {
        fake.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| {
                matches!(
                    c,
                    FakeCall::SavedSearchPut { .. } | FakeCall::SavedSearchRemove { .. }
                )
            })
            .cloned()
            .collect()
    }

    /// The open confirmation's body lines.
    fn confirm_body(screen: &MailScreen) -> Vec<String> {
        match &screen.modal {
            Some(Modal::Confirm(confirm)) => confirm.lines().to_vec(),
            _ => panic!("no confirmation is open"),
        }
    }

    /// The command an open modal answers `key` with.
    fn answer(screen: &mut MailScreen, code: KeyCode) -> Command {
        let outcome = screen
            .modal
            .as_mut()
            .expect("the modal is still open")
            .key(key(code));
        match outcome {
            ModalOutcome::Run(command) => command,
            other => panic!("{code:?} did not answer the modal: {other:?}"),
        }
    }

    /// A confirmed removal can only ever remove the search the question named. `d` resolves
    /// the row, then the answer to a write already in flight replaces the list underneath the
    /// open question — `MailEvent::SavedSearchesChanged` runs whatever modal is up — so an
    /// index read at `y` time would name the next search along and remove a search the user
    /// never confirmed.
    #[tokio::test]
    async fn a_confirmed_removal_never_hits_a_search_that_moved() {
        let (_tmp, fake, mut screen) = screen_with(&["a-list", "b-list", "c-list"]);
        screen.remove_search_confirm(0);
        let asked = confirm_body(&screen);
        assert!(asked[0].contains("\"a-list\""), "{asked:?}");

        let effects = screen.saved_searches_changed(Ok(searches(&["b-list", "c-list"])));
        settle(&mut screen, effects).await;

        let command = answer(&mut screen, KeyCode::Char('y'));
        let effects = screen.action_command(command);
        settle(&mut screen, effects).await;

        assert_eq!(saved_calls(&fake), Vec::new(), "nothing was removed");
        assert!(
            screen
                .status_line()
                .contains("The saved searches changed; try again."),
            "{}",
            screen.status_line()
        );
    }

    /// The same for a rename, whose window is far wider: the prompt stays open while the name
    /// is typed, so an earlier write's answer has seconds in which to land.
    #[tokio::test]
    async fn a_rename_never_hits_a_search_that_moved() {
        let (_tmp, fake, mut screen) = screen_with(&["a-list", "b-list", "c-list"]);
        screen.rename_search_prompt(0);
        let effects = screen.saved_searches_changed(Ok(searches(&["b-list", "c-list"])));
        settle(&mut screen, effects).await;

        screen
            .modal
            .as_mut()
            .expect("the prompt is still open")
            .key(key(KeyCode::Char('s')));
        let command = answer(&mut screen, KeyCode::Enter);
        let effects = screen.action_command(command);
        settle(&mut screen, effects).await;

        assert_eq!(saved_calls(&fake), Vec::new(), "nothing was renamed");
        assert_eq!(screen.pending_rename, None, "no rename is remembered");
        assert!(
            screen
                .status_line()
                .contains("The saved searches changed; try again."),
            "{}",
            screen.status_line()
        );
    }
}
