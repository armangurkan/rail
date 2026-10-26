//! Snooze over a bulk selection: the notice counts what the answers snoozed and left out, a call
//! refused as "not in the inbox" counts as left out, and only a snooze none of whose calls took
//! anything says the refusal. A child module of `tests/snooze.rs`, whose helpers it uses.

use rail_core::ops::{Action as MailAction, ActionApplyResponse, NOT_IN_INBOX, RowToken};

use super::super::super::selection::Picked;
use super::*;
use crate::tui::data::ApplyCall;

/// The chooser's first choice at `NOW`: "Later today", 18:00.
const LATER: i64 = MIDNIGHT + 18 * HOUR;

async fn feed(f: &mut Fixture, event: MailEvent) {
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
}

/// `* a`: every thread of the scope.
async fn select_all(f: &mut Fixture) {
    press(f, KeyCode::Char('*')).await;
    press(f, KeyCode::Char('a')).await;
}

/// The selection's thread ids of the account with this store id, ascending.
fn selected_of(f: &Fixture, account_id: i64) -> Vec<i64> {
    let mut ids: Vec<i64> = f
        .screen
        .actions
        .selection
        .targets()
        .iter()
        .filter(|(_, picked)| picked.account_id == account_id)
        .map(|(id, _)| *id)
        .collect();
    ids.sort_unstable();
    ids
}

/// `h`, then `Enter` on the first choice with its `snooze.set` calls left unrun, so each answer
/// can be fed by hand: the snooze's calls are seq 1.
async fn snooze_unanswered(f: &mut Fixture) {
    press(f, KeyCode::Char('h')).await;
    let _unrun = f.screen.update(Msg::Key(key(KeyCode::Enter)));
}

/// One `snooze.set` call's answer: `Ok((rows, snoozed))` or the daemon's error.
fn answer(
    account: &str,
    threads: Vec<i64>,
    result: Result<(Vec<i64>, usize), OpError>,
) -> MailEvent {
    MailEvent::Applied {
        call: Box::new(ApplyCall {
            seq: 1,
            account: account.into(),
            threads,
            action: MailAction::Archive {},
            grace: true,
            client_token: format!("tok-{account}"),
            snooze: Some(LATER),
        }),
        result: result.map(|(rows, applied)| ActionApplyResponse { rows, applied }),
    }
}

fn not_in_inbox() -> OpError {
    OpError::invalid_input(NOT_IN_INBOX)
}

#[tokio::test]
async fn a_bulk_snooze_counts_its_threads_in_the_notice() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('1')).await;
    select_all(&mut f).await;
    press(&mut f, KeyCode::Char('h')).await;
    press(&mut f, KeyCode::Enter).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Snoozed 3 until Fri 18:00 · z undoes"),
        "{line}"
    );
}

#[tokio::test]
async fn each_answer_adds_what_it_snoozed_and_left_out() {
    let mut f = fixture(2).await;
    select_all(&mut f).await;
    let (work, home) = (selected_of(&f, 1), selected_of(&f, 2));
    snooze_unanswered(&mut f).await;
    // home answers first: one of its two threads had nothing in the inbox.
    feed(&mut f, answer("home", home, Ok((vec![11], 1)))).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Snoozed 1 until Fri 18:00 · z undoes · 1 not in the inbox"),
        "{line}"
    );
    feed(&mut f, answer("work", work, Ok((vec![10], 2)))).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Snoozed 3 until Fri 18:00 · z undoes · 1 not in the inbox"),
        "{line}"
    );
}

#[tokio::test]
async fn a_call_refused_as_not_in_the_inbox_counts_as_left_out() {
    let mut f = fixture(2).await;
    select_all(&mut f).await;
    let (work, home) = (selected_of(&f, 1), selected_of(&f, 2));
    snooze_unanswered(&mut f).await;
    feed(&mut f, answer("work", work, Err(not_in_inbox()))).await;
    let line = f.screen.status_line();
    assert!(!line.contains("Could not"), "{line}");
    feed(&mut f, answer("home", home, Ok((vec![11], 2)))).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Snoozed 2 until Fri 18:00 · z undoes · 2 not in the inbox"),
        "{line}"
    );
    assert_eq!(
        f.screen.actions.selection.len(),
        0,
        "left out, not put back"
    );
}

#[tokio::test]
async fn a_snooze_whose_every_call_was_refused_says_so_once_and_gives_the_selection_back() {
    let mut f = fixture(2).await;
    select_all(&mut f).await;
    let (work, home) = (selected_of(&f, 1), selected_of(&f, 2));
    snooze_unanswered(&mut f).await;
    feed(&mut f, answer("work", work, Err(not_in_inbox()))).await;
    assert!(!f.screen.status_line().contains("Could not"));
    feed(&mut f, answer("home", home, Err(not_in_inbox()))).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Could not snooze: Only a thread in the inbox can be snoozed."),
        "{line}"
    );
    assert_eq!(f.screen.actions.selection.len(), 4);
}

#[tokio::test]
async fn a_four_digit_count_keeps_the_undo_hint_at_90_columns() {
    let mut f = fixture(2).await;
    let picked = Picked {
        account_id: 1,
        starred: false,
        unread: true,
    };
    let ids: Vec<i64> = (1_000..2_300).collect();
    let targets: Vec<(i64, Picked)> = ids.iter().map(|id| (*id, picked)).collect();
    assert!(!f.screen.actions.selection.add(&targets));
    snooze_unanswered(&mut f).await;
    // Seven calls of up to 200; the last snoozed 34 of its 100, so 1,234 in all.
    let chunks: Vec<Vec<i64>> = ids.chunks(200).map(<[i64]>::to_vec).collect();
    assert_eq!(chunks.len(), 7);
    for (row, chunk) in (10..).zip(chunks) {
        let snoozed = if chunk.len() == 100 { 34 } else { chunk.len() };
        feed(&mut f, answer("work", chunk, Ok((vec![row], snoozed)))).await;
    }
    let shown = drawn(&f);
    assert!(
        shown.contains("Snoozed 1,234 until Fri 18:00 · z undoes · 66 not in the inbox"),
        "{shown}"
    );
}

#[tokio::test]
async fn a_refusal_after_the_snooze_took_some_adds_to_its_count() {
    let mut f = fixture(2).await;
    select_all(&mut f).await;
    let (work, home) = (selected_of(&f, 1), selected_of(&f, 2));
    snooze_unanswered(&mut f).await;
    feed(&mut f, answer("home", home, Ok((vec![11], 2)))).await;
    feed(&mut f, answer("work", work, Err(not_in_inbox()))).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Snoozed 2 until Fri 18:00 · z undoes · 2 not in the inbox"),
        "{line}"
    );
}

#[tokio::test]
async fn a_snooze_that_took_nothing_gives_back_its_refused_threads_with_the_failed_ones() {
    let mut f = fixture(2).await;
    select_all(&mut f).await;
    let (work, home) = (selected_of(&f, 1), selected_of(&f, 2));
    snooze_unanswered(&mut f).await;
    feed(&mut f, answer("work", work, Err(not_in_inbox()))).await;
    let failed = Err(OpError::not_found("the daemon said no"));
    feed(&mut f, answer("home", home, failed)).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Could not snooze: the daemon said no"),
        "{line}"
    );
    assert_eq!(f.screen.actions.selection.len(), 4, "both calls' threads");
}

#[tokio::test]
async fn a_failed_call_keeps_its_error_on_the_bar_while_the_others_answer() {
    let mut f = fixture(2).await;
    select_all(&mut f).await;
    let (work, home) = (selected_of(&f, 1), selected_of(&f, 2));
    snooze_unanswered(&mut f).await;
    let failed = Err(OpError::not_found("the daemon said no"));
    feed(&mut f, answer("work", work.clone(), failed)).await;
    feed(&mut f, answer("home", home, Ok((vec![11], 2)))).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Could not snooze: the daemon said no"),
        "{line}"
    );
    assert_eq!(
        selected_of(&f, 1),
        work,
        "only the failed call's threads are back"
    );
    assert_eq!(f.screen.actions.selection.len(), 2);
    press(&mut f, KeyCode::Char('z')).await;
    let undone: Vec<FakeCall> = f
        .fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| matches!(c, FakeCall::Undo { .. }))
        .cloned()
        .collect();
    let home_row = RowToken {
        row: 11,
        client_token: "tok-home".into(),
    };
    assert_eq!(
        undone,
        [FakeCall::Undo {
            rows: vec![home_row]
        }]
    );
}
