//! The sidebar on its own: walking it, folding, what `Enter` does on each kind of row, pointing
//! at a scope, its counts, and keeping its place across rebuilds.

use super::AccountEntry;
use super::Sidebar;
use super::sidebar_tree::fixtures::{folder, two_accounts};
use crate::tui::data::ScopeChoice;
use crate::tui::test_util::work_and_home;

/// `work` and `home` labelled as the fixtures' accounts are.
fn accounts() -> Vec<AccountEntry> {
    let mut accounts = work_and_home();
    accounts[0].label = "Personal".into();
    accounts[1].label = "AXON".into();
    accounts
}

fn sidebar() -> Sidebar {
    let mut sidebar = Sidebar::default();
    sidebar.rebuild(&accounts(), &two_accounts());
    sidebar
}

fn here(s: &Sidebar) -> Option<&str> {
    s.selected().map(|r| r.label.as_str())
}

/// Move the cursor down to the first row labelled `label`.
fn walk_to(s: &mut Sidebar, label: &str) {
    for _ in 0..s.rows().len() {
        if here(s) == Some(label) {
            return;
        }
        s.down();
    }
    panic!("no row {label:?} below the cursor");
}

#[test]
fn the_cursor_starts_on_all_inboxes_and_skips_headings_and_rules() {
    let mut s = sidebar();
    assert_eq!(here(&s), Some("All inboxes"));
    s.up();
    assert_eq!(
        here(&s),
        Some("All inboxes"),
        "nothing above it but the heading"
    );
    walk_to(&mut s, "Trash");
    s.down();
    assert_eq!(
        here(&s),
        Some("Personal"),
        "the rule and ACCOUNTS are skipped"
    );
    for _ in 0..s.rows().len() {
        s.down();
    }
    assert_eq!(here(&s), Some("More folders (5)"), "the last row");
    s.up();
    assert_eq!(
        here(&s),
        Some("Deleted Items"),
        "the rule above it is skipped"
    );
}

#[test]
fn space_folds_accounts_more_folders_and_tree_nodes_and_nothing_else() {
    let mut s = sidebar();
    let shown = s.rows().len();
    s.toggle_fold();
    assert_eq!(s.rows().len(), shown, "All inboxes does not fold");
    walk_to(&mut s, "Personal");
    s.toggle_fold();
    assert!(
        s.selected()
            .is_some_and(|r| r.label == "Personal" && r.folded)
    );
    assert_eq!(
        s.rows().len(),
        shown - 7,
        "five system folders, the rule and More folders"
    );
    s.toggle_fold();
    assert_eq!(s.rows().len(), shown);
    walk_to(&mut s, "More folders (7)");
    s.toggle_fold();
    assert_eq!(
        s.rows().len(),
        shown + 4,
        "[Imap], Categories, Important, Shopping"
    );
    s.down();
    assert_eq!(here(&s), Some("[Imap]"));
    s.toggle_fold();
    assert_eq!(s.rows().len(), shown + 6);
    s.down();
    assert_eq!(here(&s), Some("Sent"));
    s.toggle_fold();
    assert_eq!(
        s.rows().len(),
        shown + 6,
        "a folder with nothing under it does not fold"
    );
}

#[test]
fn enter_opens_by_row_kind_and_unfolds_where_there_is_nothing_to_open() {
    let mut s = sidebar();
    walk_to(&mut s, "Starred");
    assert_eq!(s.open(), Some(ScopeChoice::Starred));
    walk_to(&mut s, "Personal");
    assert_eq!(
        s.open(),
        Some(ScopeChoice::Account(0)),
        "an account opens its inbox"
    );
    walk_to(&mut s, "Inbox");
    assert_eq!(
        s.open(),
        Some(ScopeChoice::Account(0)),
        "so does its Inbox row"
    );
    walk_to(&mut s, "More folders (7)");
    assert_eq!(s.open(), None);
    assert!(
        s.selected()
            .is_some_and(|r| r.label == "More folders (7)" && !r.folded),
        "unfolded in place, the cursor stays"
    );
    s.down();
    assert_eq!(here(&s), Some("[Imap]"));
    assert_eq!(s.open(), None, "a path with no folder of its own unfolds");
    assert!(s.selected().is_some_and(|r| !r.folded));
    walk_to(&mut s, "Shopping");
    let opened = s.open();
    assert!(
        matches!(&opened, Some(ScopeChoice::Folder { account: 0, name, .. }) if name == "Shopping"),
        "a path that is a folder opens it: {opened:?}"
    );
}

/// Review Focus 4 and 5.
#[test]
fn point_at_unfolds_what_hides_the_scope_and_keeps_a_row_that_already_shows_it() {
    let mut s = sidebar();
    let linode = ScopeChoice::Folder {
        account: 1,
        folder_id: 40,
        name: "Inbox/Infrastructure/Linode".into(),
    };
    s.point_at(&linode);
    assert_eq!(here(&s), Some("Linode"));
    let labels: Vec<&str> = s.rows().iter().map(|r| r.label.as_str()).collect();
    assert!(
        labels.contains(&"Hetzner") && labels.contains(&"Outbox"),
        "More folders, Inbox and Infrastructure unfolded: {labels:?}"
    );
    let mut s = sidebar();
    walk_to(&mut s, "Personal");
    s.point_at(&ScopeChoice::Account(0));
    assert_eq!(
        here(&s),
        Some("Personal"),
        "the account row already shows its inbox"
    );
    s.point_at(&ScopeChoice::Account(1));
    assert_eq!(
        s.selected().map(|r| (r.label.as_str(), r.account())),
        Some(("Inbox", Some(1))),
        "else the account's Inbox row"
    );
}

#[test]
fn unread_serves_the_header_from_the_counts() {
    let s = sidebar();
    assert_eq!(s.unread(&ScopeChoice::Unified), 15);
    assert_eq!(s.unread(&ScopeChoice::Starred), 12);
    assert_eq!(s.unread(&ScopeChoice::Account(1)), 10);
    let linode = ScopeChoice::Folder {
        account: 1,
        folder_id: 40,
        name: String::new(),
    };
    assert_eq!(s.unread(&linode), 4, "folded away or not");
    assert_eq!(s.unread(&ScopeChoice::Sent), 0);
    assert_eq!(
        Sidebar::default().unread(&ScopeChoice::Unified),
        0,
        "nothing read yet"
    );
}

/// Review Focus 3.
#[test]
fn a_rebuild_keeps_the_folds_and_the_cursor_or_the_nearest_row_above() {
    let mut s = sidebar();
    walk_to(&mut s, "More folders (7)");
    s.toggle_fold();
    walk_to(&mut s, "Shopping");
    s.toggle_fold();
    walk_to(&mut s, "dhgate");
    let mut outcome = two_accounts();
    outcome
        .counts
        .folders
        .push(folder(1, 23, "Shopping/aliexpress", "user", 1));
    s.rebuild(&accounts(), &outcome);
    assert_eq!(
        here(&s),
        Some("dhgate"),
        "a new folder does not move the cursor"
    );
    assert!(
        s.rows().iter().any(|r| r.label == "aliexpress"),
        "Shopping stays unfolded"
    );
    outcome
        .counts
        .folders
        .retain(|f| f.name != "Shopping/dhgate");
    s.rebuild(&accounts(), &outcome);
    assert_eq!(here(&s), Some("aliexpress"), "gone: the nearest row above");
}
