//! The sidebar: the merged mailboxes, each account with its system folders and its other
//! folders under `More folders`, folding for the session, and a cursor kept on its row across
//! re-reads. The rows come from `sidebar_tree`.

use std::cell::Cell;
use std::collections::HashSet;

use rail_core::store::view::MAX_SIDEBAR_FOLDERS;

use super::sidebar_tree::{self, Folds, RowKey, SidebarRow, TreeAccount};
use super::{AccountEntry, MailScreen};
use crate::tui::app::Effect;
use crate::tui::data::{ReadError, ScopeChoice, SidebarOutcome};

#[derive(Debug, Default)]
pub struct Sidebar {
    /// Every row, folded away or not, in order.
    all: Vec<SidebarRow>,
    /// The rows shown: no folded row above them.
    rows: Vec<SidebarRow>,
    cursor: Option<RowKey>,
    folds: Folds,
    /// The accounts' labels and addresses and the counts the rows came from, kept for a fold.
    accounts: Vec<TreeAccount>,
    /// The saved searches' names, in config order: kept so a fold can rebuild without them.
    saved: Vec<String>,
    outcome: Option<SidebarOutcome>,
    /// The last read failed: the old rows stay, under a note.
    failed: bool,
    /// Accounts (by index) whose folder list was cut at `MAX_SIDEBAR_FOLDERS`.
    capped: Vec<usize>,
    /// The folders that are an account's Drafts.
    drafts: HashSet<i64>,
    /// The first row drawn, from the last frame.
    offset: Cell<usize>,
    /// The rows the last frame drew: a page.
    height: Cell<usize>,
}

impl Sidebar {
    /// The rows shown.
    pub fn rows(&self) -> &[SidebarRow] {
        &self.rows
    }

    /// Every row, folded away or not.
    pub fn all_rows(&self) -> &[SidebarRow] {
        &self.all
    }

    /// The cursor's index in `rows`: its row, else the first row it can stop on.
    pub fn cursor(&self) -> usize {
        self.cursor
            .as_ref()
            .and_then(|k| self.rows.iter().position(|r| &r.key == k))
            .or_else(|| self.rows.iter().position(SidebarRow::selectable))
            .unwrap_or(0)
    }

    /// The rows for `accounts` and `saved` (both in config order) from fresh counts; the folds
    /// and the cursor stay.
    pub fn rebuild(
        &mut self,
        accounts: &[AccountEntry],
        saved: &[String],
        outcome: &SidebarOutcome,
    ) {
        self.saved = saved.to_vec();
        self.accounts = accounts
            .iter()
            .map(|a| TreeAccount {
                label: a.label.clone(),
                address: a.address.clone(),
            })
            .collect();
        self.capped = capped(accounts.len(), outcome);
        self.drafts = outcome
            .counts
            .folders
            .iter()
            .filter(|f| f.kind == "drafts")
            .map(|f| f.folder_id)
            .collect();
        self.outcome = Some(outcome.clone());
        self.failed = false;
        self.relayout();
    }

    /// Build the rows again from the kept counts and the folds; the cursor stays on its row or
    /// goes to the nearest one above.
    fn relayout(&mut self) {
        let Some(outcome) = &self.outcome else {
            return;
        };
        let all = sidebar_tree::build(&self.accounts, &self.saved, outcome, &self.folds);
        let rows = sidebar_tree::visible(&all, &self.folds);
        self.cursor = sidebar_tree::refind(&self.rows, self.cursor.as_ref(), &rows);
        self.all = all;
        self.rows = rows;
    }

    /// The shown row drawn as `scope`'s: the last one with it (an account's Inbox row while the
    /// account is unfolded, its account row while folded).
    pub fn current(&self, scope: &ScopeChoice) -> Option<usize> {
        self.rows
            .iter()
            .rposition(|r| r.scope().as_ref() == Some(scope))
    }

    /// The first row to draw so row `target` shows in `height` rows; kept for the next frame.
    pub fn scroll(&self, target: usize, height: usize) -> usize {
        let kept = self
            .offset
            .get()
            .min(self.rows.len().saturating_sub(height));
        let offset = super::visible_offset(kept, target, height);
        self.offset.set(offset);
        self.height.set(height);
        offset
    }

    /// The last read failed: keep the rows and say so.
    pub fn fail(&mut self) {
        self.failed = true;
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn down(&mut self) {
        self.step(1);
    }

    pub fn up(&mut self) {
        self.step(-1);
    }

    /// To the next row the cursor stops on, `by` (1 or -1) at a time; stays at either end.
    fn step(&mut self, by: isize) {
        let mut at = self.cursor();
        while let Some(next) = at.checked_add_signed(by).filter(|&i| i < self.rows.len()) {
            at = next;
            if self.rows[at].selectable() {
                self.cursor = Some(self.rows[at].key.clone());
                return;
            }
        }
    }

    pub fn page_down(&mut self) {
        self.page(true);
    }

    pub fn page_up(&mut self) {
        self.page(false);
    }

    /// The next (`forward`) or previous screen of rows, as the last frame drew them: the cursor
    /// keeps its place on the new screen, or goes to the last (first) row when the screen is
    /// already at that end. A heading or rule there gives way to the nearest row it stops on.
    fn page(&mut self, forward: bool) {
        if self.rows.is_empty() {
            return;
        }
        let height = match self.height.get() {
            0 => super::FALLBACK_HEIGHT,
            rows => rows,
        };
        let (last, bottom) = (self.rows.len() - 1, self.rows.len().saturating_sub(height));
        let offset = self.offset.get().min(bottom);
        let moved = if forward {
            (offset + height).min(bottom)
        } else {
            offset.saturating_sub(height)
        };
        let at = match (moved == offset, forward) {
            (true, true) => last,
            (true, false) => 0,
            (false, _) => (self.cursor() + moved).saturating_sub(offset).min(last),
        };
        let by = if forward { 1 } else { -1 };
        if let Some(stop) = self.stop_near(at, by).or_else(|| self.stop_near(at, -by)) {
            self.offset.set(moved);
            self.cursor = Some(self.rows[stop].key.clone());
        }
    }

    /// The first row from `at` on, `by` (1 or -1) at a time, that the cursor stops on.
    fn stop_near(&self, at: usize, by: isize) -> Option<usize> {
        std::iter::successors(Some(at), |i| i.checked_add_signed(by))
            .take_while(|&i| i < self.rows.len())
            .find(|&i| self.rows[i].selectable())
    }

    pub fn selected(&self) -> Option<&SidebarRow> {
        self.rows.get(self.cursor())
    }

    /// `Space`: fold or unfold the cursor's row when it folds.
    pub fn toggle_fold(&mut self) {
        let Some((key, folded)) = self
            .selected()
            .filter(|r| r.foldable)
            .map(|r| (r.key.clone(), r.folded))
        else {
            return;
        };
        self.folds.set(&key, !folded);
        self.relayout();
    }

    /// `Enter`: the scope to show; `None` when the row unfolds in place instead (`More
    /// folders`, a path with no folder of its own).
    pub fn open(&mut self) -> Option<ScopeChoice> {
        let row = self.selected()?;
        if let Some(scope) = row.scope() {
            return Some(scope);
        }
        let unfold = (row.foldable && row.folded).then(|| row.key.clone());
        if let Some(key) = unfold {
            self.folds.set(&key, false);
            self.relayout();
        }
        None
    }

    /// Put the cursor on `scope`'s row. It stays where it is when its row already shows
    /// `scope` (an account row and its Inbox row both do); else it goes to the last shown row
    /// with that scope (a folded account's row, not its hidden Inbox). Only when no shown row
    /// has it does it unfold what hides the row.
    pub fn point_at(&mut self, scope: &ScopeChoice) {
        if self.selected().and_then(SidebarRow::scope).as_ref() == Some(scope) {
            return;
        }
        if let Some(shown) = self.current(scope) {
            self.cursor = Some(self.rows[shown].key.clone());
            return;
        }
        let Some(row) = self
            .all
            .iter()
            .rev()
            .find(|r| r.scope().as_ref() == Some(scope))
        else {
            return;
        };
        let (key, parents) = (row.key.clone(), row.parents.clone());
        for parent in &parents {
            self.folds.set(parent, false);
        }
        self.cursor = Some(key);
        self.relayout();
    }

    /// A renamed saved search keeps the cursor. A saved row's key **is** its name, so the
    /// cursor's key is renamed before the rows are built again; otherwise `refind` would read
    /// the rename as a removal and drop the cursor to the row above it.
    pub fn rename_cursor(&mut self, old: &str, new: &str) {
        if self.cursor.as_ref() == Some(&RowKey::Saved(old.to_string())) {
            self.cursor = Some(RowKey::Saved(new.to_string()));
        }
    }

    /// `scope` as the sidebar has it now (a renamed label's new name), shown or folded away.
    pub fn find(&self, scope: &ScopeChoice) -> Option<ScopeChoice> {
        self.all
            .iter()
            .filter_map(SidebarRow::scope)
            .find(|s| s == scope)
    }

    /// True when `account`'s folder list reached the cap, so a folder missing from it may be
    /// beyond the cap rather than deleted.
    pub fn is_capped(&self, account: usize) -> bool {
        self.capped.contains(&account)
    }

    /// The folders (by store id) that are an account's Drafts: they show the drafts pane.
    pub fn drafts_folder_ids(&self) -> &HashSet<i64> {
        &self.drafts
    }

    /// The unread count the header shows for `scope`: from the counts, shown or folded away.
    pub fn unread(&self, scope: &ScopeChoice) -> u64 {
        let Some(outcome) = &self.outcome else {
            return 0;
        };
        let counts = &outcome.counts;
        let id_of = |i: usize| outcome.account_ids.get(i).copied().flatten();
        match scope {
            ScopeChoice::Unified => counts.accounts.iter().map(|a| a.unread).sum(),
            ScopeChoice::Starred => counts.unified.starred_unread,
            ScopeChoice::Account(i) => counts
                .accounts
                .iter()
                .find(|a| Some(a.account_id) == id_of(*i))
                .map_or(0, |a| a.unread),
            ScopeChoice::Folder { folder_id, .. } => counts
                .folders
                .iter()
                .find(|f| f.folder_id == *folder_id)
                .map_or(0, |f| f.unread),
            ScopeChoice::Sent | ScopeChoice::Trash | ScopeChoice::Drafts | ScopeChoice::Snoozed => {
                0
            }
            // Positional, in config order; empty while the counts are off.
            ScopeChoice::Saved { index, .. } => counts.saved.get(*index).copied().unwrap_or(0),
        }
    }
}

/// The accounts (by index) whose folder list reached `MAX_SIDEBAR_FOLDERS`.
fn capped(accounts: usize, outcome: &SidebarOutcome) -> Vec<usize> {
    (0..accounts)
        .filter(|&i| {
            let id = outcome.account_ids.get(i).copied().flatten();
            let folders = outcome.counts.folders.iter();
            folders.filter(|f| Some(f.account_id) == id).count() >= MAX_SIDEBAR_FOLDERS
        })
        .collect()
}

impl MailScreen {
    pub(super) fn sidebar_counted(
        &mut self,
        seq: u64,
        result: Result<SidebarOutcome, ReadError>,
    ) -> Vec<Effect> {
        if seq != self.latest_sidebar {
            return Vec::new();
        }
        match result {
            Ok(outcome) => {
                let saved: Vec<String> = self.saved.iter().map(|s| s.name.clone()).collect();
                self.sidebar.rebuild(&self.accounts, &saved, &outcome);
                self.follow_scope()
            }
            Err(error) => {
                if self.side_read_failed(error, "sidebar").is_some() {
                    self.sidebar.fail();
                }
                Vec::new()
            }
        }
    }

    /// After a rebuild: a label's scope takes the label's current name (renamed in Gmail),
    /// or, when the label is gone (or beyond the sidebar's cap), gives way to its account's
    /// inbox.
    fn follow_scope(&mut self) -> Vec<Effect> {
        let ScopeChoice::Folder { account, .. } = self.scope else {
            return Vec::new();
        };
        if let Some(scope) = self.sidebar.find(&self.scope) {
            self.scope = scope;
            return Vec::new();
        }
        let notice = if self.sidebar.is_capped(account) {
            format!(
                "This account has more than {MAX_SIDEBAR_FOLDERS} labels and the one you had \
                 open is not listed; showing the inbox."
            )
        } else {
            "The label you had open is gone; showing the inbox.".to_string()
        };
        self.status.notify(notice, self.tick);
        self.set_scope(ScopeChoice::Account(account))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §4 (B4): before the first rebuild sets it, the cursor is the first row it can
    /// stop on, never a heading.
    #[test]
    fn with_no_cursor_yet_the_cursor_is_the_first_selectable_row() {
        let folds = Folds::default();
        let all = sidebar_tree::build(
            &sidebar_tree::fixtures::accounts(),
            &[],
            &sidebar_tree::fixtures::two_accounts(),
            &folds,
        );
        let s = Sidebar {
            rows: sidebar_tree::visible(&all, &folds),
            ..Sidebar::default()
        };
        assert_eq!(s.cursor(), 1, "All inboxes, under the UNIFIED heading");
        assert_eq!(s.selected().map(|r| r.label.as_str()), Some("All inboxes"));
        assert_eq!(Sidebar::default().cursor(), 0, "no rows at all");
    }
}
