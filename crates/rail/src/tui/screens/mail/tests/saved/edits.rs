//! Renaming and removing a saved search on the mail screen: `r`, `d` and its confirmation,
//! the palette's remove entry, a row that is not the cursor's, and a failed write.
//! AC-14 … AC-18.

use super::*;

/// AC-14 (FR-14): `r` renames the search under the cursor, keeping its query.
#[tokio::test]
async fn r_renames_the_saved_search_under_the_cursor() {
    let mut f = saved_fixture(&five_threads(), &TWO).await;
    sidebar_to(&mut f, None, "needs-reply").await;
    press(&mut f, KeyCode::Char('r')).await;
    press_n(&mut f, KeyCode::Backspace, 11).await;
    type_text(&mut f, "later").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        saved_calls(&f),
        [FakeCall::SavedSearchPut {
            name: "later".into(),
            query: "from:dana is:unread".into(),
            replaces: Some("needs-reply".into()),
        }]
    );
}

/// AC-14b (FR-4's `replaces` carve-out): no edit at all, and a case change, are both accepted.
#[tokio::test]
async fn renaming_to_the_same_or_a_case_changed_name_is_allowed() {
    let mut f = saved_fixture(&five_threads(), &TWO).await;
    sidebar_to(&mut f, None, "needs-reply").await;
    press(&mut f, KeyCode::Char('r')).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(refusal(&f), None);
    assert_eq!(
        saved_calls(&f),
        [FakeCall::SavedSearchPut {
            name: "needs-reply".into(),
            query: "from:dana is:unread".into(),
            replaces: Some("needs-reply".into()),
        }]
    );

    let mut f = saved_fixture(&five_threads(), &TWO).await;
    sidebar_to(&mut f, None, "needs-reply").await;
    press(&mut f, KeyCode::Char('r')).await;
    press_n(&mut f, KeyCode::Backspace, 11).await;
    type_text(&mut f, "Needs-Reply").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        saved_calls(&f),
        [FakeCall::SavedSearchPut {
            name: "Needs-Reply".into(),
            query: "from:dana is:unread".into(),
            replaces: Some("needs-reply".into()),
        }]
    );
}

/// AC-14c (FR-17 step 1, D-5): a rename follows the scope, the header and the cursor, quietly.
#[tokio::test]
async fn renaming_the_open_search_follows_it() {
    let mut f = saved_fixture(&five_threads(), &TWO).await;
    sidebar_to(&mut f, None, "needs-reply").await;
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Tab).await;
    press(&mut f, KeyCode::Char('r')).await;
    press_n(&mut f, KeyCode::Backspace, 11).await;
    type_text(&mut f, "later").await;
    press(&mut f, KeyCode::Enter).await;
    assert!(
        matches!(&f.screen.scope, ScopeChoice::Saved { name, .. } if name == "later"),
        "{:?}",
        f.screen.scope
    );
    assert!(
        f.screen.header_left().starts_with("▍Rail │ Search: later"),
        "{}",
        f.screen.header_left()
    );
    assert_eq!(f.screen.status.notice(), None, "a rename is not a removal");
    assert!(
        f.screen
            .sidebar
            .selected()
            .is_some_and(|r| r.label == "later"),
        "{:?}",
        f.screen.sidebar.selected()
    );
}

/// AC-15 (FR-15): `d` asks first, removes on `y` and does nothing on `n`.
#[tokio::test]
async fn d_asks_then_removes_the_saved_search() {
    let mut f = saved_fixture(&five_threads(), &TWO).await;
    sidebar_to(&mut f, None, "needs-reply").await;
    press(&mut f, KeyCode::Char('d')).await;
    assert_eq!(confirm_lines(&f), [REMOVE_LINE]);
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(
        saved_calls(&f),
        [FakeCall::SavedSearchRemove {
            name: "needs-reply".into(),
        }]
    );

    let mut f = saved_fixture(&five_threads(), &TWO).await;
    sidebar_to(&mut f, None, "needs-reply").await;
    press(&mut f, KeyCode::Char('d')).await;
    press(&mut f, KeyCode::Char('n')).await;
    assert!(f.screen.modal.is_none());
    assert!(saved_calls(&f).is_empty());
}

/// AC-15b (Q-5): the palette's indexed entry reaches the same confirmation as `d`.
#[tokio::test]
async fn the_palette_opens_the_remove_confirmation() {
    let mut f = saved_fixture(&five_threads(), &TWO).await;
    run_from_the_palette(&mut f, "remove-search needs-reply").await;
    assert_eq!(confirm_lines(&f), [REMOVE_LINE]);
}

/// AC-16 (FR-16): `r` and `d` on any other sidebar row say so, never silently nothing.
#[tokio::test]
async fn r_and_d_on_another_row_say_so() {
    let mut f = saved_fixture(&five_threads(), &TWO).await;
    sidebar_to(&mut f, None, "Starred").await;
    for key in ['r', 'd'] {
        press(&mut f, KeyCode::Char(key)).await;
        assert!(f.screen.modal.is_none(), "{key} opened something");
        assert!(
            f.screen
                .status_line()
                .contains("That is not a saved search."),
            "{key}: {}",
            f.screen.status_line()
        );
    }
    assert!(saved_calls(&f).is_empty());
}

/// AC-17 (FR-17 steps 3-4): the scope falls back and the rows leave (what `read_sidebar` buys).
#[tokio::test]
async fn removing_the_open_search_falls_back_to_all_inboxes() {
    let mut f = saved_fixture(&five_threads(), &TWO[..1]).await;
    sidebar_to(&mut f, None, "needs-reply").await;
    press(&mut f, KeyCode::Enter).await;
    *f.fake.saved_answer.lock().unwrap() = Some(Ok(SavedSearchListResponse { searches: vec![] }));
    press(&mut f, KeyCode::Tab).await;
    press(&mut f, KeyCode::Char('d')).await;
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(f.screen.scope, ScopeChoice::Unified);
    assert!(
        f.screen
            .status_line()
            .contains("The saved search you had open is gone; showing All inboxes."),
        "{}",
        f.screen.status_line()
    );
    let rows = f.screen.sidebar.rows();
    assert!(!rows.iter().any(|r| matches!(r.kind, RowKind::Saved { .. })));
    assert!(!rows.iter().any(|r| r.key == RowKey::Heading("SEARCHES")));
    // Tab above showed the sidebar at 90 columns, so this is not vacuous.
    let shown = drawn(&f, 90, 24);
    assert!(shown.contains("ACCOUNTS"), "the sidebar is drawn: {shown}");
    assert!(
        !shown
            .lines()
            .any(|line| line.split('│').next().unwrap_or(line).trim_end() == "SEARCHES"),
        "{shown}"
    );
}

/// AC-17's other half (FR-17 step 3): removing the search **above** the open one keeps it
/// open and re-keys it to where it now is, so the next read is still its own and nothing is
/// called gone. `TWO` lists `needs-reply` first, so with it removed `big-attachments` moves
/// from index 1 to index 0 — the shift a positional scope has to be told about.
#[tokio::test]
async fn removing_the_search_above_the_open_one_shifts_its_index() {
    let mut f = saved_fixture(&five_threads(), &TWO).await;
    sidebar_to(&mut f, None, "big-attachments").await;
    press(&mut f, KeyCode::Enter).await;
    // Matched, not compared: `ScopeChoice`'s `PartialEq` reads a `Saved`'s `index` only.
    assert!(
        matches!(&f.screen.scope, ScopeChoice::Saved { index: 1, name } if name == "big-attachments"),
        "{:?}",
        f.screen.scope
    );

    *f.fake.saved_answer.lock().unwrap() = Some(Ok(SavedSearchListResponse {
        searches: vec![SavedSearch {
            name: "big-attachments".into(),
            query: "has:attachment".into(),
        }],
    }));
    sidebar_up_to(&mut f, None, "needs-reply").await;
    press(&mut f, KeyCode::Char('d')).await;
    press(&mut f, KeyCode::Char('y')).await;

    assert!(
        matches!(&f.screen.scope, ScopeChoice::Saved { index: 0, name } if name == "big-attachments"),
        "{:?}",
        f.screen.scope
    );
    assert!(
        f.screen
            .header_left()
            .starts_with("▍Rail │ Search: big-attachments"),
        "{}",
        f.screen.header_left()
    );
    assert_eq!(
        f.screen.status.notice(),
        None,
        "the open search was not read as the removed one"
    );
}

/// AC-18 (FR-18): a failed write keeps the list and says why, `detail` included.
#[tokio::test]
async fn a_failed_write_keeps_the_list_and_says_why() {
    let mut f = saved_fixture(&five_threads(), &TWO).await;
    *f.fake.saved_answer.lock().unwrap() = Some(Err(OpError::not_found(
        "There is no saved search named \"gone\".",
    )
    .with_detail("saved_searches[0]")));
    sidebar_to(&mut f, None, "needs-reply").await;
    press(&mut f, KeyCode::Char('r')).await;
    press_n(&mut f, KeyCode::Backspace, 11).await;
    type_text(&mut f, "later").await;
    press(&mut f, KeyCode::Enter).await;
    let line = f.screen.status_line();
    assert!(
        line.contains(
            "Could not change the saved searches: There is no saved search named \"gone\"."
        ),
        "{line}"
    );
    assert!(
        line.contains("from outside Rail (the provider or the system): \"saved_searches[0]\""),
        "{line}"
    );
    assert!(
        f.screen
            .sidebar
            .rows()
            .iter()
            .any(|r| r.key == RowKey::Saved("needs-reply".into())),
        "the old name is still listed"
    );
}
