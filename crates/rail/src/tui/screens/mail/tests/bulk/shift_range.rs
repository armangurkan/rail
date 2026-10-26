//! `J`/`K` and Shift-↓/↑ through the mail screen: the first press anchors on the cursor row,
//! each press selects from the anchor to the cursor, moving back deselects what the range
//! added, any other key ends it, `v` range mode keeps the keys to moving, drafts refuse them,
//! the cap holds, and help and the palette list them. A child module of `tests/bulk.rs`, whose
//! helpers it uses.

use super::super::drafts_folder::{drafts_fixture, open_drafts};
use super::*;

/// `code` with Shift held, as a terminal that reports it sends Shift-↓.
async fn press_shift(f: &mut Fixture, code: KeyCode) {
    let event = crossterm::event::KeyEvent::new(code, crossterm::event::KeyModifiers::SHIFT);
    let effects = f.screen.update(Msg::Key(event));
    settle_capturing(&mut f.screen, effects, &f.last_theme, &f.editor).await;
}

fn sorted(ids: impl IntoIterator<Item = i64>) -> Vec<i64> {
    let mut ids: Vec<i64> = ids.into_iter().collect();
    ids.sort_unstable();
    ids
}

#[tokio::test]
async fn j_extends_the_selection_down_and_k_takes_it_back() {
    let mut f = fixture(6).await;
    let rows = thread_ids(&f);
    press_n(&mut f, KeyCode::Char('J'), 2).await;
    assert_eq!(selected_ids(&f), sorted(rows[0..=2].iter().copied()));
    assert_eq!(f.screen.list.selected_index(), 2);
    press(&mut f, KeyCode::Char('K')).await;
    assert_eq!(selected_ids(&f), sorted(rows[0..=1].iter().copied()));
}

#[tokio::test]
async fn shrinking_keeps_what_was_selected_before_the_range() {
    let mut f = fixture(6).await;
    let rows = thread_ids(&f);
    press_n(&mut f, KeyCode::Char('j'), 2).await;
    press(&mut f, KeyCode::Char(' ')).await;
    press_n(&mut f, KeyCode::Char('k'), 2).await;
    press_n(&mut f, KeyCode::Char('J'), 2).await;
    assert_eq!(selected_ids(&f), sorted(rows[1..=3].iter().copied()));
    press_n(&mut f, KeyCode::Char('K'), 2).await;
    assert_eq!(
        selected_ids(&f),
        sorted([rows[1], rows[2]]),
        "row 2 was selected first"
    );
}

#[tokio::test]
async fn any_other_key_ends_the_range_and_keeps_the_selection() {
    let mut f = fixture(6).await;
    let rows = thread_ids(&f);
    press(&mut f, KeyCode::Char('J')).await;
    press_n(&mut f, KeyCode::Char('j'), 2).await;
    // A new range anchored on row 3 takes in row 2.
    press(&mut f, KeyCode::Char('K')).await;
    assert_eq!(selected_ids(&f), sorted(rows[0..=3].iter().copied()));
    press(&mut f, KeyCode::Char('J')).await;
    assert_eq!(selected_ids(&f), sorted([rows[0], rows[1], rows[3]]));
}

#[tokio::test]
async fn shift_arrows_extend_as_j_and_k_do() {
    let mut f = fixture(6).await;
    let rows = thread_ids(&f);
    press_shift(&mut f, KeyCode::Down).await;
    press_shift(&mut f, KeyCode::Down).await;
    press_shift(&mut f, KeyCode::Up).await;
    assert_eq!(selected_ids(&f), sorted(rows[0..=1].iter().copied()));
}

#[tokio::test]
async fn at_the_last_row_extending_down_selects_it_and_stays() {
    let mut f = fixture(3).await;
    let rows = thread_ids(&f);
    let last = rows.len() - 1;
    press_n(&mut f, KeyCode::Char('j'), last).await;
    press(&mut f, KeyCode::Char('J')).await;
    assert_eq!(selected_ids(&f), [rows[last]]);
    assert_eq!(f.screen.list.selected_index(), last);
}

#[tokio::test]
async fn in_range_mode_the_capitals_only_move() {
    let mut f = fixture(6).await;
    let rows = thread_ids(&f);
    press(&mut f, KeyCode::Char('v')).await;
    press_n(&mut f, KeyCode::Char('J'), 2).await;
    assert!(
        selected_ids(&f).is_empty(),
        "v keeps its range until it ends"
    );
    press(&mut f, KeyCode::Char('v')).await;
    assert_eq!(selected_ids(&f), sorted(rows[0..=2].iter().copied()));
}

#[tokio::test]
async fn drafts_refuse_extending_the_selection() {
    let mut f = drafts_fixture().await;
    open_drafts(&mut f).await;
    press(&mut f, KeyCode::Char('J')).await;
    let line = f.screen.status_line();
    assert!(line.contains("Not available on drafts"), "{line}");
}

#[tokio::test]
async fn a_full_selection_says_so_and_stops_growing() {
    let mut f = fixture(6).await;
    let filler: Vec<(i64, Picked)> = (0..MAX_SELECTION - 1)
        .map(|i| {
            let id = 1_000_000 + i64::try_from(i).unwrap();
            let picked = Picked {
                account_id: 1,
                starred: false,
                unread: false,
            };
            (id, picked)
        })
        .collect();
    f.screen.actions.selection.add(&filler);
    press_n(&mut f, KeyCode::Char('J'), 2).await;
    assert_eq!(f.screen.actions.selection.len(), MAX_SELECTION);
    let line = f.screen.status_line();
    assert!(
        line.contains("The selection holds at most 10,000 threads."),
        "{line}"
    );
}

#[tokio::test]
async fn the_extend_actions_reach_help_and_the_palette() {
    let mut f = fixture(3).await;
    let wanted = [
        (
            Action::ListExtendDown,
            "J",
            "Shift-↓",
            "Extend the selection down",
        ),
        (
            Action::ListExtendUp,
            "K",
            "Shift-↑",
            "Extend the selection up",
        ),
    ];
    press(&mut f, KeyCode::Char('?')).await;
    let Some(Modal::Help(help)) = &f.screen.modal else {
        panic!("no help is open")
    };
    let text = help.text();
    for (_, letter, arrow, description) in wanted {
        let row = text
            .lines()
            .find(|line| line.contains(description))
            .unwrap_or_else(|| panic!("{description} is not in `?`:\n{text}"));
        assert!(row.contains(letter) && row.contains(arrow), "{row}");
    }
    press(&mut f, KeyCode::Esc).await;
    let entries = f.screen.palette_entries();
    for (action, letter, _, description) in wanted {
        let entry = entries
            .iter()
            .find(|entry| entry.target == Target::Action(action))
            .unwrap_or_else(|| panic!("{description} is not in `:`"));
        assert_eq!(entry.text, description);
        assert_eq!(entry.key, letter);
    }
}
