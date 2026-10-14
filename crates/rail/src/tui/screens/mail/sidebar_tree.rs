//! The sidebar's rows, built from the store's counts: the merged mailboxes under UNIFIED, then
//! under ACCOUNTS each account with its system folders and, under `More folders (N)`, every
//! other folder as a tree of its `/`-separated path. Pure: the fold state comes in, rows go out.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used from the wiring task on; remove then")
)]

use std::collections::HashSet;

use rail_core::provider::FolderKind;
use rail_core::store::view::FolderCount;

use crate::tui::data::{ScopeChoice, SidebarOutcome};

pub const UNIFIED: &str = "UNIFIED";
pub const ACCOUNTS: &str = "ACCOUNTS";
/// Path segments a tree row shows at most: deeper ones are joined into the last.
const MAX_SEGMENTS: usize = 6;
/// The system folders: shown under the account, in the store's order.
const SYSTEM: [FolderKind; 6] = [
    FolderKind::Inbox,
    FolderKind::Sent,
    FolderKind::Drafts,
    FolderKind::Archive,
    FolderKind::Spam,
    FolderKind::Trash,
];
/// Gmail's category labels (`Category: Social`) gather under `Categories`.
const CATEGORY: &str = "Category: ";
const CATEGORIES: &str = "Categories";
/// The merged mailboxes, in the order UNIFIED lists them.
const MERGED: [ScopeChoice; 5] = [
    ScopeChoice::Unified,
    ScopeChoice::Starred,
    ScopeChoice::Drafts,
    ScopeChoice::Sent,
    ScopeChoice::Trash,
];

/// What a row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// `UNIFIED`, `ACCOUNTS`.
    Heading(&'static str),
    /// A muted line across the sidebar.
    Rule,
    /// All inboxes, Starred, Drafts, Sent, Trash.
    Unified(ScopeChoice),
    /// The account row: badge, label, inbox count.
    Account { index: usize },
    /// A folder with nothing under it (the inbox opens the account's inbox scope).
    Folder {
        account: usize,
        scope: ScopeChoice,
        kind: FolderKind,
    },
    /// `More folders (N)`.
    More { account: usize, hidden: usize },
    /// A tree node with folders under it; `scope` when it is itself a folder.
    Parent {
        account: usize,
        path: String,
        scope: Option<ScopeChoice>,
    },
}

/// A row's identity across rebuilds: the cursor and the fold state are kept by it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowKey {
    Heading(&'static str),
    /// The rule closing UNIFIED (`None`) or the one above an account's `More folders`.
    Rule(Option<usize>),
    /// A merged mailbox by its name.
    Unified(&'static str),
    Account(usize),
    More(usize),
    /// A folder by its store id: a renamed label keeps its row.
    Folder {
        account: usize,
        folder_id: i64,
    },
    /// A tree node that is only a path prefix (`[Imap]`, `Categories`).
    Prefix {
        account: usize,
        path: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarRow {
    pub kind: RowKind,
    pub key: RowKey,
    /// The account's label or the folder's last path segment: outside text, escaped when drawn.
    pub label: String,
    /// 0 headings, unified rows and accounts; 1 system folders, rules and `More folders`; 2 and
    /// deeper inside `More folders`.
    pub depth: u8,
    /// `None` draws nothing.
    pub count: Option<u64>,
    pub foldable: bool,
    pub folded: bool,
    /// The foldable rows above this one, outermost first: it shows when none is folded.
    pub parents: Vec<RowKey>,
}

impl SidebarRow {
    /// The scope `Enter` opens.
    pub fn scope(&self) -> Option<ScopeChoice> {
        match &self.kind {
            RowKind::Unified(scope) | RowKind::Folder { scope, .. } => Some(scope.clone()),
            RowKind::Parent { scope, .. } => scope.clone(),
            RowKind::Account { index } => Some(ScopeChoice::Account(*index)),
            RowKind::Heading(_) | RowKind::Rule | RowKind::More { .. } => None,
        }
    }

    /// The account the row belongs to.
    pub fn account(&self) -> Option<usize> {
        match &self.kind {
            RowKind::Account { index } => Some(*index),
            RowKind::Folder { account, .. }
            | RowKind::More { account, .. }
            | RowKind::Parent { account, .. } => Some(*account),
            RowKind::Heading(_) | RowKind::Rule | RowKind::Unified(_) => None,
        }
    }

    /// The cursor stops here: not on a heading or a rule.
    pub fn selectable(&self) -> bool {
        !matches!(self.kind, RowKind::Heading(_) | RowKind::Rule)
    }
}

/// Which rows are folded. Accounts start unfolded and every other foldable row folded; the set
/// holds the rows toggled away from that, for the session.
#[derive(Debug, Clone, Default)]
pub struct Folds(HashSet<RowKey>);

impl Folds {
    pub fn is_folded(&self, key: &RowKey) -> bool {
        folded_by_default(key) != self.0.contains(key)
    }

    pub fn set(&mut self, key: &RowKey, folded: bool) {
        if folded == folded_by_default(key) {
            self.0.remove(key);
        } else {
            self.0.insert(key.clone());
        }
    }
}

fn folded_by_default(key: &RowKey) -> bool {
    !matches!(key, RowKey::Account(_))
}

/// Every row, folded away or not, in order; `labels` are the accounts' labels in config order.
pub fn build(labels: &[String], outcome: &SidebarOutcome, folds: &Folds) -> Vec<SidebarRow> {
    let mut rows = unified_rows(outcome);
    rows.push(heading(ACCOUNTS));
    for (index, label) in labels.iter().enumerate() {
        let id = outcome.account_ids.get(index).copied().flatten();
        let own: Vec<&FolderCount> = outcome
            .counts
            .folders
            .iter()
            .filter(|f| Some(f.account_id) == id)
            .collect();
        rows.push(account_row(index, label, inbox_unread(outcome, id), folds));
        rows.extend(system_rows(index, &own, drafts_of(outcome, id)));
        rows.extend(more_rows(index, &own, folds));
    }
    rows
}

/// The rows shown: those with no folded row above them.
pub fn visible(all: &[SidebarRow], folds: &Folds) -> Vec<SidebarRow> {
    all.iter()
        .filter(|r| r.parents.iter().all(|p| !folds.is_folded(p)))
        .cloned()
        .collect()
}

/// Where the cursor goes in `new`: its own row when shown, else the nearest row above it in
/// `old` that `new` still shows, else the first row it can stop on.
pub fn refind(old: &[SidebarRow], key: Option<&RowKey>, new: &[SidebarRow]) -> Option<RowKey> {
    if let Some(key) = key.filter(|k| new.iter().any(|r| &r.key == *k)) {
        return Some(key.clone());
    }
    let at = key
        .and_then(|k| old.iter().position(|r| &r.key == k))
        .unwrap_or(0);
    old.iter()
        .take(at + 1)
        .rev()
        .filter(|r| r.selectable())
        .find(|r| new.iter().any(|n| n.key == r.key))
        .or_else(|| new.iter().find(|r| r.selectable()))
        .map(|r| r.key.clone())
}

fn row(kind: RowKind, key: RowKey, label: String, depth: u8, count: Option<u64>) -> SidebarRow {
    SidebarRow {
        kind,
        key,
        label,
        depth,
        count,
        foldable: false,
        folded: false,
        parents: Vec::new(),
    }
}

fn heading(text: &'static str) -> SidebarRow {
    row(
        RowKind::Heading(text),
        RowKey::Heading(text),
        text.to_string(),
        0,
        None,
    )
}

/// UNIFIED, its five mailboxes and the rule under them. Sent and Trash count nothing.
fn unified_rows(outcome: &SidebarOutcome) -> Vec<SidebarRow> {
    let counts = &outcome.counts;
    let mut rows = vec![heading(UNIFIED)];
    for scope in MERGED {
        let name = scope.unified_name().unwrap_or_default();
        let count = match scope {
            ScopeChoice::Unified => Some(counts.accounts.iter().map(|a| a.unread).sum()),
            ScopeChoice::Starred => Some(counts.unified.starred_unread),
            ScopeChoice::Drafts => Some(counts.unified.drafts),
            _ => None,
        };
        let key = RowKey::Unified(name);
        rows.push(row(
            RowKind::Unified(scope),
            key,
            name.to_string(),
            0,
            count,
        ));
    }
    rows.push(row(
        RowKind::Rule,
        RowKey::Rule(None),
        String::new(),
        0,
        None,
    ));
    rows
}

fn inbox_unread(outcome: &SidebarOutcome, id: Option<i64>) -> u64 {
    outcome
        .counts
        .accounts
        .iter()
        .find(|a| Some(a.account_id) == id)
        .map_or(0, |a| a.unread)
}

fn account_row(index: usize, label: &str, unread: u64, folds: &Folds) -> SidebarRow {
    let key = RowKey::Account(index);
    SidebarRow {
        foldable: true,
        folded: folds.is_folded(&key),
        ..row(
            RowKind::Account { index },
            key,
            label.to_string(),
            0,
            Some(unread),
        )
    }
}

fn kind_of(f: &FolderCount) -> FolderKind {
    FolderKind::parse(&f.kind).unwrap_or(FolderKind::User)
}

/// The inbox opens the account's inbox scope; any other folder its own.
fn folder_scope(account: usize, f: &FolderCount) -> ScopeChoice {
    if kind_of(f) == FolderKind::Inbox {
        ScopeChoice::Account(account)
    } else {
        ScopeChoice::Folder {
            account,
            folder_id: f.folder_id,
            name: f.name.clone(),
        }
    }
}

/// Unread threads for most folders, nothing for Sent and Trash; the Drafts count comes from
/// `drafts_of` in `system_rows`, so none here.
fn folder_count(kind: FolderKind, f: &FolderCount) -> Option<u64> {
    match kind {
        FolderKind::Sent | FolderKind::Trash | FolderKind::Drafts => None,
        _ => Some(f.unread),
    }
}

fn folder_row(account: usize, f: &FolderCount, label: String, depth: u8) -> SidebarRow {
    let kind = kind_of(f);
    let key = RowKey::Folder {
        account,
        folder_id: f.folder_id,
    };
    let scope = folder_scope(account, f);
    row(
        RowKind::Folder {
            account,
            scope,
            kind,
        },
        key,
        label,
        depth,
        folder_count(kind, f),
    )
}

/// The account's system folders at depth 1, in the store's order (inbox, sent, drafts,
/// archive, spam, trash), under the provider's own names; the Drafts row counts `drafts`, what
/// the Drafts pane shows for the account.
fn system_rows(index: usize, own: &[&FolderCount], drafts: u64) -> Vec<SidebarRow> {
    own.iter()
        .filter(|f| SYSTEM.contains(&kind_of(f)))
        .map(|f| {
            let row = folder_row(index, f, f.name.clone(), 1);
            let count = if kind_of(f) == FolderKind::Drafts {
                Some(drafts)
            } else {
                row.count
            };
            SidebarRow {
                parents: vec![RowKey::Account(index)],
                count,
                ..row
            }
        })
        .collect()
}

/// The account's drafts (local and provider) from the counts; 0 when it has no store id yet.
fn drafts_of(outcome: &SidebarOutcome, id: Option<i64>) -> u64 {
    outcome
        .counts
        .unified
        .drafts_by_account
        .iter()
        .find(|(account, _)| Some(*account) == id)
        .map_or(0, |(_, n)| *n)
}

/// The rule, `More folders (N)` and, under it, every other folder as a tree; nothing when the
/// account has no other folder.
fn more_rows(index: usize, own: &[&FolderCount], folds: &Folds) -> Vec<SidebarRow> {
    let other: Vec<&FolderCount> = own
        .iter()
        .copied()
        .filter(|f| !SYSTEM.contains(&kind_of(f)))
        .collect();
    if other.is_empty() {
        return Vec::new();
    }
    let account = RowKey::Account(index);
    let more = RowKey::More(index);
    let rule = SidebarRow {
        parents: vec![account.clone()],
        ..row(
            RowKind::Rule,
            RowKey::Rule(Some(index)),
            String::new(),
            1,
            None,
        )
    };
    let kind = RowKind::More {
        account: index,
        hidden: other.len(),
    };
    let label = format!("More folders ({})", other.len());
    let head = SidebarRow {
        foldable: true,
        folded: folds.is_folded(&more),
        parents: vec![account.clone()],
        ..row(kind, more.clone(), label, 1, None)
    };
    let mut rows = vec![rule, head];
    Node::tree(&other).emit(index, 2, &[account, more], folds, &mut rows);
    rows
}

/// A folder's place in the tree: `Category: X` is `Categories`, `X`; any other name is split
/// at `/`, segments past `MAX_SEGMENTS` joined into the last (`a/b/c/d/e/f/g` is `a`…`e`,
/// `f/g`).
fn segments(f: &FolderCount) -> Vec<String> {
    if kind_of(f) == FolderKind::Label
        && let Some(category) = f.name.strip_prefix(CATEGORY)
    {
        return vec![CATEGORIES.to_string(), category.to_string()];
    }
    let parts: Vec<&str> = f.name.split('/').collect();
    if parts.len() <= MAX_SEGMENTS {
        return parts.into_iter().map(str::to_string).collect();
    }
    let (head, tail) = parts.split_at(MAX_SEGMENTS - 1);
    head.iter()
        .map(|s| (*s).to_string())
        .chain(std::iter::once(tail.join("/")))
        .collect()
}

/// One node of the tree under `More folders`: a path segment, the folder it is (if any), its
/// children sorted by name ignoring case.
#[derive(Debug, Default)]
struct Node<'a> {
    name: String,
    path: String,
    folder: Option<&'a FolderCount>,
    children: Vec<Node<'a>>,
}

impl<'a> Node<'a> {
    fn tree(folders: &[&'a FolderCount]) -> Node<'a> {
        let mut root = Node::default();
        for &f in folders {
            root.insert(&segments(f), f);
        }
        root.sort();
        root
    }

    fn insert(&mut self, segments: &[String], folder: &'a FolderCount) {
        let Some((first, rest)) = segments.split_first() else {
            return;
        };
        let found = self
            .children
            .iter()
            .position(|c| c.name == *first && (!rest.is_empty() || c.folder.is_none()));
        let at = match found {
            Some(at) => at,
            None => {
                let path = if self.path.is_empty() {
                    first.clone()
                } else {
                    format!("{}/{first}", self.path)
                };
                self.children.push(Node {
                    name: first.clone(),
                    path,
                    ..Node::default()
                });
                self.children.len() - 1
            }
        };
        let child = &mut self.children[at];
        if rest.is_empty() {
            child.folder = Some(folder);
        } else {
            child.insert(rest, folder);
        }
    }

    fn sort(&mut self) {
        self.children.sort_by_key(|c| c.name.to_lowercase());
        self.children.iter_mut().for_each(Node::sort);
    }

    /// The unread threads of this node's folder and of every folder under it.
    fn unread_sum(&self) -> u64 {
        self.folder.map_or(0, |f| f.unread)
            + self.children.iter().map(Node::unread_sum).sum::<u64>()
    }

    /// This node's children as rows at `depth`, each under `parents`, then theirs.
    fn emit(
        &self,
        account: usize,
        depth: u8,
        parents: &[RowKey],
        folds: &Folds,
        out: &mut Vec<SidebarRow>,
    ) {
        for child in &self.children {
            let row = child.row(account, depth, parents, folds);
            let mut inner = parents.to_vec();
            inner.push(row.key.clone());
            out.push(row);
            child.emit(account, depth.saturating_add(1), &inner, folds, out);
        }
    }

    /// A leaf folder's row, or a parent's: foldable, its unread summed while folded, its own
    /// (or none) while unfolded.
    fn row(&self, account: usize, depth: u8, parents: &[RowKey], folds: &Folds) -> SidebarRow {
        if let (Some(f), true) = (self.folder, self.children.is_empty()) {
            return SidebarRow {
                parents: parents.to_vec(),
                ..folder_row(account, f, self.name.clone(), depth)
            };
        }
        let key = match self.folder {
            Some(f) => RowKey::Folder {
                account,
                folder_id: f.folder_id,
            },
            None => RowKey::Prefix {
                account,
                path: self.path.clone(),
            },
        };
        let folded = folds.is_folded(&key);
        let count = if folded {
            Some(self.unread_sum())
        } else {
            self.folder.map(|f| f.unread)
        };
        let kind = RowKind::Parent {
            account,
            path: self.path.clone(),
            scope: self.folder.map(|f| folder_scope(account, f)),
        };
        SidebarRow {
            foldable: true,
            folded,
            parents: parents.to_vec(),
            ..row(kind, key, self.name.clone(), depth, count)
        }
    }
}

#[cfg(test)]
pub(super) mod fixtures {
    use rail_core::store::view::{AccountCounts, FolderCount, SidebarCounts, UnifiedCounts};

    use crate::tui::data::SidebarOutcome;

    /// A folder of store account `account_id` with `unread` unread threads and one more.
    pub fn folder(
        account_id: i64,
        folder_id: i64,
        name: &str,
        kind: &str,
        unread: u64,
    ) -> FolderCount {
        FolderCount {
            folder_id,
            account_id,
            name: name.into(),
            kind: kind.into(),
            threads: unread + 1,
            unread,
        }
    }

    /// The Gmail account's folders: (folder id, name, kind, unread).
    const GMAIL: [(i64, &str, &str, u64); 12] = [
        (11, "Inbox", "inbox", 5),
        (12, "Sent", "sent", 0),
        (13, "Drafts", "drafts", 0),
        (14, "Spam", "spam", 0),
        (15, "Trash", "trash", 0),
        (16, "Shopping", "user", 1),
        (17, "Shopping/dhgate", "user", 2),
        (18, "[Imap]/Sent", "user", 0),
        (19, "[Imap]/Trash", "user", 0),
        (20, "Category: Social", "label", 3),
        (21, "Category: Updates", "label", 4),
        (22, "Important", "label", 0),
    ];
    /// The Microsoft 365 account's folders: (folder id, name, kind, unread).
    const M365: [(i64, &str, &str, u64); 11] = [
        (31, "Inbox", "inbox", 10),
        (32, "Sent Items", "sent", 0),
        (33, "Drafts", "drafts", 0),
        (34, "Archive", "archive", 0),
        (35, "Junk Email", "spam", 0),
        (36, "Deleted Items", "trash", 0),
        (37, "1 - Saved", "user", 0),
        (38, "Inbox/Infrastructure", "user", 1),
        (39, "Inbox/Infrastructure/Hetzner", "user", 2),
        (40, "Inbox/Infrastructure/Linode", "user", 4),
        (41, "Outbox", "user", 0),
    ];

    /// The user's two shapes, in the store's order (system folders by kind, then the rest):
    /// a Gmail account (store id 1) with categories, `[Imap]/…` and `Shopping/dhgate`, and a
    /// Microsoft 365 one (store id 2) with `Inbox/Infrastructure/…`, `Outbox` and `1 - Saved`.
    pub fn two_accounts() -> SidebarOutcome {
        let folders = GMAIL
            .iter()
            .map(|&(id, name, kind, unread)| folder(1, id, name, kind, unread))
            .chain(
                M365.iter()
                    .map(|&(id, name, kind, unread)| folder(2, id, name, kind, unread)),
            )
            .collect();
        SidebarOutcome {
            counts: SidebarCounts {
                accounts: vec![
                    AccountCounts {
                        account_id: 1,
                        threads: 20,
                        unread: 5,
                    },
                    AccountCounts {
                        account_id: 2,
                        threads: 40,
                        unread: 10,
                    },
                ],
                folders,
                unified: UnifiedCounts {
                    starred_unread: 12,
                    drafts: 3,
                    drafts_by_account: vec![(1, 2), (2, 1)],
                },
            },
            account_ids: vec![Some(1), Some(2)],
        }
    }

    /// The two accounts' labels.
    pub fn labels() -> Vec<String> {
        vec!["Personal".into(), "AXON".into()]
    }
}
