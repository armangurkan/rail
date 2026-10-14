//! The sidebar through the mail screen's keys: walking it, folding, opening each kind of row,
//! and keeping its place across re-reads.

use crossterm::event::KeyCode;

use super::drafts_folder::{
    LOCAL_AT, gmail_draft, pane_subjects, recipient, seed_local, sync_drafts,
};
use super::state::labels;
use super::{Fixture, fixture, press, store_changed, writer};
use crate::tui::data::ScopeChoice;
use crate::tui::screens::mail::Focus;
use crate::tui::test_util::{BASE_MS, settle};

/// Focus the sidebar and move the cursor down to the row labelled `label` of `account`.
pub(super) async fn sidebar_to(f: &mut Fixture, account: Option<usize>, label: &str) {
    if f.screen.focus != Focus::Sidebar {
        press(f, KeyCode::Tab).await;
    }
    for _ in 0..f.screen.sidebar.rows().len() {
        let here = f
            .screen
            .sidebar
            .selected()
            .is_some_and(|r| r.label == label && r.account() == account);
        if here {
            return;
        }
        press(f, KeyCode::Char('j')).await;
    }
    panic!("no sidebar row {label:?} of {account:?} below the cursor");
}

/// Unfold `work`'s More folders and open Projects; the list then has focus.
pub(super) async fn open_projects(f: &mut Fixture) {
    sidebar_to(f, Some(0), "More folders (1)").await;
    press(f, KeyCode::Char(' ')).await;
    press(f, KeyCode::Char('j')).await;
    press(f, KeyCode::Enter).await;
}

fn rows(f: &Fixture) -> usize {
    f.screen.sidebar.rows().len()
}

fn here(f: &Fixture) -> Option<(&str, Option<usize>)> {
    f.screen
        .sidebar
        .selected()
        .map(|r| (r.label.as_str(), r.account()))
}

#[tokio::test]
async fn enter_and_space_act_on_each_kind_of_row() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Tab).await;
    let shown = rows(&f);
    press(&mut f, KeyCode::Char(' ')).await;
    assert_eq!(rows(&f), shown, "All inboxes does not fold");
    sidebar_to(&mut f, None, "Starred").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(f.screen.scope, ScopeChoice::Starred);
    assert_eq!(f.screen.focus, Focus::List);
    sidebar_to(&mut f, Some(0), "work").await;
    press(&mut f, KeyCode::Char(' ')).await;
    let folded = shown - 4;
    assert_eq!(
        rows(&f),
        folded,
        "work's Inbox, Sent, rule and More folders fold away"
    );
    press(&mut f, KeyCode::Right).await;
    assert_eq!(rows(&f), shown, "→ unfolds it again");
    press(&mut f, KeyCode::Left).await;
    assert_eq!(rows(&f), folded, "← folds it");
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        f.screen.scope,
        ScopeChoice::Account(0),
        "an account opens its inbox"
    );
    sidebar_to(&mut f, Some(0), "More folders (1)").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        f.screen.focus,
        Focus::Sidebar,
        "More folders unfolds in place"
    );
    assert_eq!(f.screen.scope, ScopeChoice::Account(0));
    assert!(
        f.screen
            .sidebar
            .selected()
            .is_some_and(|r| r.label == "More folders (1)" && !r.folded)
    );
    press(&mut f, KeyCode::Char('j')).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(
        matches!(&f.screen.scope, ScopeChoice::Folder { account: 0, name, .. } if name == "Projects")
    );
}

/// `→` and `←` are `sidebar.fold`'s other keys: on a folded More folders row `→` unfolds it and
/// `←` folds it again, the cursor staying on the row and the scope unchanged.
#[tokio::test]
async fn right_and_left_unfold_and_fold_through_the_keymap() {
    let mut f = fixture(3).await;
    sidebar_to(&mut f, Some(1), "More folders (1)").await;
    let shown = rows(&f);
    press(&mut f, KeyCode::Right).await;
    assert_eq!(rows(&f), shown + 1, "home's Projects shows");
    assert_eq!(here(&f), Some(("More folders (1)", Some(1))));
    assert!(f.screen.sidebar.selected().is_some_and(|r| !r.folded));
    press(&mut f, KeyCode::Left).await;
    assert_eq!(rows(&f), shown, "folded again");
    assert!(f.screen.sidebar.selected().is_some_and(|r| r.folded));
    assert_eq!(f.screen.focus, Focus::Sidebar);
    assert_eq!(f.screen.scope, ScopeChoice::Unified);
}

/// Review Focus 5.
#[tokio::test]
async fn enter_on_an_inbox_row_leaves_the_cursor_there() {
    let mut f = fixture(3).await;
    sidebar_to(&mut f, Some(1), "Inbox").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(f.screen.scope, ScopeChoice::Account(1));
    assert_eq!(
        here(&f),
        Some(("Inbox", Some(1))),
        "not moved to the account row"
    );
    press(&mut f, KeyCode::Char('1')).await;
    assert_eq!(
        here(&f),
        Some(("Inbox", Some(0))),
        "`1` puts it on work's Inbox"
    );
}

#[tokio::test]
async fn number_keys_still_choose_a_mailbox_from_the_sidebar() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Tab).await;
    press(&mut f, KeyCode::Char('2')).await;
    assert_eq!(f.screen.scope, ScopeChoice::Account(1));
    assert_eq!(f.screen.focus, Focus::Sidebar, "the sidebar keeps the keys");
    assert_eq!(here(&f), Some(("Inbox", Some(1))));
    press(&mut f, KeyCode::Char('0')).await;
    assert_eq!(f.screen.scope, ScopeChoice::Unified);
    assert_eq!(here(&f), Some(("All inboxes", None)));
}

#[tokio::test]
async fn b_hides_the_sidebar_and_gives_the_list_the_keys() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Tab).await;
    press(&mut f, KeyCode::Char('b')).await;
    assert!(!f.screen.sidebar_visible());
    assert_eq!(f.screen.focus, Focus::List);
    press(&mut f, KeyCode::Char(' ')).await;
    assert_eq!(
        f.screen.actions.selection.len(),
        1,
        "Space selects a thread again, it does not fold"
    );
}

/// Review Focus 3.
#[tokio::test]
async fn the_cursor_and_the_scope_survive_a_folder_added_and_removed() {
    let mut f = fixture(3).await;
    let mut store = writer(&f);
    open_projects(&mut f).await;
    press(&mut f, KeyCode::Tab).await;
    store
        .apply(
            &labels(&[("Label_0", "Alpha"), ("Label_1", "Projects")]),
            BASE_MS / 1000,
        )
        .unwrap();
    store_changed(&mut f).await;
    let projects = |f: &Fixture| matches!(&f.screen.scope, ScopeChoice::Folder { account: 0, name, .. } if name == "Projects");
    assert!(projects(&f), "{:?}", f.screen.scope);
    assert_eq!(here(&f), Some(("Projects", Some(0))));
    assert!(
        f.screen.sidebar.rows().iter().any(|r| r.label == "Alpha"),
        "More folders stayed open"
    );
    store
        .apply(&labels(&[("Label_1", "Projects")]), BASE_MS / 1000)
        .unwrap();
    store_changed(&mut f).await;
    assert!(projects(&f));
    assert_eq!(here(&f), Some(("Projects", Some(0))));
    assert_eq!(f.screen.header_left(), "▍Rail │ work · Projects · 0 unread");
}

/// Review Focus 4.
#[tokio::test]
async fn a_label_from_the_palette_unfolds_its_way_in_the_sidebar() {
    let mut f = fixture(3).await;
    let entry = f
        .screen
        .palette_entries()
        .into_iter()
        .find(|e| e.text == "Label: work · Projects")
        .expect("Projects is in the palette while folded away");
    let effects = f.screen.run_palette(entry.target);
    settle(&mut f.screen, effects).await;
    assert_eq!(here(&f), Some(("Projects", Some(0))));
    assert!(
        f.screen
            .sidebar
            .rows()
            .iter()
            .any(|r| r.label == "More folders (1)" && r.account() == Some(0) && !r.folded),
        "work's More folders unfolded"
    );
    assert_eq!(f.screen.header_left(), "▍Rail │ work · Projects · 0 unread");
}

/// The UNIFIED Drafts row opens one Drafts pane over every account: `home`'s local draft and
/// `work`'s Gmail draft, newest first.
#[tokio::test]
async fn the_unified_drafts_row_lists_every_accounts_drafts() {
    let mut f = fixture(3).await;
    let to = recipient(None, "kim@example.com");
    seed_local(&f, 1, 2, "Home errand", to, LOCAL_AT + 600);
    let gmail = gmail_draft("dm1", "Written on the phone", "gail@example.com", LOCAL_AT);
    sync_drafts(&f, vec![gmail]);
    store_changed(&mut f).await;
    sidebar_to(&mut f, None, "Drafts").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(f.screen.scope, ScopeChoice::Drafts);
    assert!(f.screen.in_drafts());
    assert_eq!(
        pane_subjects(&f),
        ["Home errand", "Written on the phone"],
        "both accounts' drafts, newest first"
    );
}
