//! The sidebar through the mail screen's keys: walking it, folding, opening each kind of row,
//! and keeping its place across re-reads.

use crossterm::event::KeyCode;

use super::{Fixture, press};
use crate::tui::screens::mail::Focus;

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
