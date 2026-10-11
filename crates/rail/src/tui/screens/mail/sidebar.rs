//! The sidebar: the unified inbox, each account's inbox, and each account's other folders,
//! with unread counts, and a cursor that stays on its mailbox across re-reads.

use rail_core::store::view::MAX_SIDEBAR_FOLDERS;

use super::{AccountEntry, MailScreen};
use crate::tui::app::Effect;
use crate::tui::data::{ReadError, ScopeChoice, SidebarOutcome};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarItem {
    pub scope: ScopeChoice,
    /// An account's name or a folder's name: outside text for folders.
    pub label: String,
    pub unread: u64,
    /// 0 for the unified inbox and accounts, 1 for folders.
    pub depth: u8,
    pub account: Option<usize>,
}

#[derive(Debug, Default)]
pub struct Sidebar {
    items: Vec<SidebarItem>,
    cursor: usize,
    /// The last read failed: the old items stay, under a note.
    failed: bool,
    /// Accounts (by index) whose folder list was cut at `MAX_SIDEBAR_FOLDERS`.
    capped: Vec<usize>,
}

impl Sidebar {
    pub fn items(&self) -> &[SidebarItem] {
        &self.items
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The items for `accounts` (in order) from fresh counts, the cursor kept on its scope.
    pub fn rebuild(&mut self, accounts: &[AccountEntry], outcome: &SidebarOutcome) {
        let at = self.items.get(self.cursor).map(|i| i.scope.clone());
        let mut capped = Vec::new();
        let mut items = vec![SidebarItem {
            scope: ScopeChoice::Unified,
            label: "All inboxes".to_string(),
            unread: outcome.counts.accounts.iter().map(|a| a.unread).sum(),
            depth: 0,
            account: None,
        }];
        for (index, account) in accounts.iter().enumerate() {
            let id = outcome.account_ids.get(index).copied().flatten();
            let unread = outcome
                .counts
                .accounts
                .iter()
                .find(|a| Some(a.account_id) == id)
                .map_or(0, |a| a.unread);
            items.push(SidebarItem {
                scope: ScopeChoice::Account(index),
                label: account.name.clone(),
                unread,
                depth: 0,
                account: Some(index),
            });
            let own = || {
                outcome
                    .counts
                    .folders
                    .iter()
                    .filter(|f| Some(f.account_id) == id)
            };
            if own().count() >= MAX_SIDEBAR_FOLDERS {
                capped.push(index);
            }
            let folders = own().filter(|f| f.kind != "inbox");
            items.extend(folders.map(|f| SidebarItem {
                scope: ScopeChoice::Folder {
                    account: index,
                    folder_id: f.folder_id,
                    name: f.name.clone(),
                },
                label: f.name.clone(),
                unread: f.unread,
                depth: 1,
                account: Some(index),
            }));
        }
        self.cursor = at
            .and_then(|scope| items.iter().position(|i| i.scope == scope))
            .unwrap_or(0);
        self.items = items;
        self.capped = capped;
        self.failed = false;
    }

    /// The last read failed: keep the items and say so.
    pub fn fail(&mut self) {
        self.failed = true;
    }

    pub fn failed(&self) -> bool {
        self.failed
    }

    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.items.len().saturating_sub(1));
    }

    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn selected(&self) -> Option<&SidebarItem> {
        self.items.get(self.cursor)
    }

    /// Put the cursor on `scope`, if it is listed.
    pub fn point_at(&mut self, scope: &ScopeChoice) {
        if let Some(index) = self.items.iter().position(|i| i.scope == *scope) {
            self.cursor = index;
        }
    }

    /// True when `account`'s folder list reached the cap, so a folder missing from it may be
    /// beyond the cap rather than deleted.
    pub fn is_capped(&self, account: usize) -> bool {
        self.capped.contains(&account)
    }

    /// The unread count shown for `scope`.
    pub fn unread(&self, scope: &ScopeChoice) -> u64 {
        self.items
            .iter()
            .find(|i| i.scope == *scope)
            .map_or(0, |i| i.unread)
    }
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
                self.sidebar.rebuild(&self.accounts, &outcome);
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
        if let Some(item) = self.sidebar.items().iter().find(|i| i.scope == self.scope) {
            self.scope = item.scope.clone();
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
