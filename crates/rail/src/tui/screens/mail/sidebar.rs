//! The sidebar: the unified inbox, each account's inbox, and each account's other folders,
//! with unread counts, and a cursor that stays on its mailbox across re-reads.

use crate::tui::data::{ScopeChoice, SidebarOutcome};

use super::AccountEntry;

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
            let folders = outcome
                .counts
                .folders
                .iter()
                .filter(|f| Some(f.account_id) == id && f.kind != "inbox");
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

    /// The unread count shown for `scope`.
    pub fn unread(&self, scope: &ScopeChoice) -> u64 {
        self.items
            .iter()
            .find(|i| i.scope == *scope)
            .map_or(0, |i| i.unread)
    }
}
