//! Snooze over a bulk selection: the notice counts what the answers snoozed and left out, a call
//! refused as "not in the inbox" counts as left out, and only a snooze none of whose calls took
//! anything says the refusal. A child module of `tests/snooze.rs`, whose helpers it uses.

use rail_core::ops::{
    Action as MailAction, ActionApplyResponse, ActionUndoResponse, NOT_IN_INBOX, RowToken,
    SnoozeClearResponse,
};

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
async fn a_four_digit_count_keeps_the_undo_hint_and_a_narrow_bar_cuts_the_count() {
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
    // At 70 columns the left-out count is what goes.
    let narrow = render_lines(70, 24, |frame| f.screen.view(frame, &Theme::default())).join("\n");
    assert!(
        narrow.contains("Snoozed 1,234 until Fri 18:00 · z undoes"),
        "{narrow}"
    );
    assert!(!narrow.contains("66 not in the inbox"), "{narrow}");
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

/// Work's first `n` threads, snoozed in the store until tomorrow 08:00 and put in the selection
/// as a bulk select does (the list read before still shows them).
fn snoozed_selection(f: &mut Fixture, n: usize) -> Vec<i64> {
    let ids: Vec<i64> = {
        let store = writer(f);
        let mut statement = store
            .conn()
            .prepare("SELECT id FROM threads WHERE account_id = 1 ORDER BY id LIMIT ?1")
            .unwrap();
        statement
            .query_map([i64::try_from(n).unwrap()], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };
    for id in &ids {
        snoozed_in_store(f, *id, MIDNIGHT + DAY + 8 * HOUR);
    }
    let picked = Picked {
        account_id: 1,
        starred: false,
        unread: true,
    };
    let targets: Vec<(i64, Picked)> = ids.iter().map(|id| (*id, picked)).collect();
    assert!(!f.screen.actions.selection.add(&targets));
    ids
}

/// The fake's `snooze.clear` calls: work's thread lists, in order.
fn clears(f: &Fixture) -> Vec<Vec<i64>> {
    snoozes(f)
        .into_iter()
        .map(|call| match call {
            FakeCall::SnoozeClear { account, threads } => {
                assert_eq!(account, "work");
                threads
            }
            other => panic!("{other:?}"),
        })
        .collect()
}

#[tokio::test]
async fn return_to_inbox_now_goes_in_chunks_of_two_hundred_and_sums_their_answers() {
    let mut f = fixture(250).await;
    let ids = snoozed_selection(&mut f, 250);
    press(&mut f, KeyCode::Char('h')).await;
    // "Return to inbox now" is the first row.
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(clears(&f), [ids[..200].to_vec(), ids[200..].to_vec()]);
    let line = f.screen.status_line();
    assert!(line.contains("returned 250 to the inbox"), "{line}");
}

#[tokio::test]
async fn a_failed_return_call_puts_its_threads_back_and_keeps_its_error() {
    let mut f = fixture(250).await;
    let ids = snoozed_selection(&mut f, 250);
    f.fake.clear_answers.lock().unwrap().extend([
        Err(OpError::not_found("the daemon said no")),
        Ok(SnoozeClearResponse { returned: 50 }),
    ]);
    press(&mut f, KeyCode::Char('h')).await;
    press(&mut f, KeyCode::Enter).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Could not return to the inbox: the daemon said no"),
        "{line}"
    );
    assert_eq!(selected_of(&f, 1), ids[..200], "the failed call's threads");
}

/// One `snooze.clear` call of the Return with `seq` answered: `returned` of `threads`.
fn returned(seq: u64, threads: Vec<i64>, returned: usize) -> MailEvent {
    MailEvent::Unsnoozed {
        seq,
        threads,
        result: Ok(SnoozeClearResponse { returned }),
    }
}

#[tokio::test]
async fn an_older_returns_answer_is_not_counted() {
    let mut f = fixture(2).await;
    let ids = snoozed_selection(&mut f, 2);
    press(&mut f, KeyCode::Char('h')).await;
    let _unrun = f.screen.update(Msg::Key(key(KeyCode::Enter)));
    let older = f.screen.seq;
    // The same threads again, before the first Return answered.
    let picked = Picked {
        account_id: 1,
        starred: false,
        unread: true,
    };
    let targets: Vec<(i64, Picked)> = ids.iter().map(|id| (*id, picked)).collect();
    assert!(!f.screen.actions.selection.add(&targets));
    press(&mut f, KeyCode::Char('h')).await;
    let _unrun = f.screen.update(Msg::Key(key(KeyCode::Enter)));
    let newer = f.screen.seq;
    assert_ne!(older, newer);
    feed(&mut f, returned(older, ids.clone(), 2)).await;
    let line = f.screen.status_line();
    assert!(!line.contains("returned"), "{line}");
    feed(&mut f, returned(newer, ids, 2)).await;
    let line = f.screen.status_line();
    assert!(line.contains("returned 2 to the inbox"), "{line}");
}

/// The rows of each `action.undo` the fake took, in order.
fn undo_rows(f: &Fixture) -> Vec<Vec<i64>> {
    f.fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            FakeCall::Undo { rows } => Some(rows.iter().map(|r| r.row).collect()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn a_snooze_of_252_threads_goes_out_as_three_calls_and_z_takes_back_every_row() {
    let mut f = fixture(250).await;
    select_all(&mut f).await;
    assert_eq!(f.screen.actions.selection.len(), 252);
    press(&mut f, KeyCode::Char('h')).await;
    press(&mut f, KeyCode::Enter).await;
    let calls: Vec<(String, usize)> = snoozes(&f)
        .into_iter()
        .map(|call| match call {
            FakeCall::SnoozeSet {
                account,
                threads,
                until,
                grace,
            } => {
                assert_eq!((until, grace), (LATER, true));
                (account, threads.len())
            }
            other => panic!("{other:?}"),
        })
        .collect();
    let expected = [("work", 200), ("work", 50), ("home", 2)].map(|(a, n)| (a.to_string(), n));
    assert_eq!(calls, expected);
    press(&mut f, KeyCode::Char('z')).await;
    let undone: Vec<RowToken> = f
        .fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            FakeCall::Undo { rows } => Some(rows.clone()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(undone.len(), 3, "every call's row: {undone:?}");
    let tokens: BTreeSet<&str> = undone.iter().map(|r| r.client_token.as_str()).collect();
    assert_eq!(tokens.len(), 3, "each under its own call's token");
}

#[tokio::test]
async fn z_between_a_snoozes_answers_takes_back_each_row_as_it_comes() {
    let mut f = fixture(2).await;
    select_all(&mut f).await;
    let (work, home) = (selected_of(&f, 1), selected_of(&f, 2));
    snooze_unanswered(&mut f).await;
    feed(&mut f, answer("work", work, Ok((vec![10], 2)))).await;
    press(&mut f, KeyCode::Char('z')).await;
    assert_eq!(undo_rows(&f), [vec![10]]);
    assert!(f.screen.status_line().contains("undoing…"));
    // home's row was already sent by the time its undo came.
    *f.fake.undo_answer.lock().unwrap() = Some(Ok(ActionUndoResponse {
        undone: vec![],
        too_late: vec![11],
    }));
    feed(&mut f, answer("home", home, Ok((vec![11], 2)))).await;
    assert_eq!(undo_rows(&f), [vec![10], vec![11]]);
    let line = f.screen.status_line();
    assert!(
        line.contains("undone in part: some were already sent"),
        "{line}"
    );
}

#[tokio::test]
async fn z_then_every_call_refused_gives_the_threads_back_and_says_nothing_to_undo() {
    let mut f = fixture(2).await;
    select_all(&mut f).await;
    let (work, home) = (selected_of(&f, 1), selected_of(&f, 2));
    snooze_unanswered(&mut f).await;
    press(&mut f, KeyCode::Char('z')).await;
    feed(&mut f, answer("work", work, Err(not_in_inbox()))).await;
    feed(&mut f, answer("home", home, Err(not_in_inbox()))).await;
    let line = f.screen.status_line();
    assert!(line.contains("Nothing to undo."), "{line}");
    assert_eq!(f.screen.actions.selection.len(), 4);
}

#[tokio::test]
async fn a_replaced_snoozes_refused_call_leaves_the_newer_action_alone() {
    let mut f = fixture(2).await;
    select_all(&mut f).await;
    let work = selected_of(&f, 1);
    snooze_unanswered(&mut f).await;
    // A newer action takes the bar and the undo before the snooze's answers come.
    press(&mut f, KeyCode::Char('e')).await;
    let line = f.screen.status_line();
    assert!(line.contains("archived 1 · z undoes"), "{line}");
    feed(&mut f, answer("work", work, Err(not_in_inbox()))).await;
    let line = f.screen.status_line();
    assert!(line.contains("archived 1 · z undoes"), "{line}");
    assert_eq!(f.screen.actions.selection.len(), 0);
}

#[tokio::test]
async fn refused_threads_stay_left_out_when_the_snooze_took_some_whatever_answers_last() {
    let mut f = fixture(2).await;
    let pick = |account_id| Picked {
        account_id,
        starred: false,
        unread: true,
    };
    // Three calls: work's 200 and 1, home's 2.
    let work: Vec<i64> = (1..=201).collect();
    let home = vec![1_001, 1_002];
    let targets: Vec<(i64, Picked)> = (work.iter().map(|id| (*id, pick(1))))
        .chain(home.iter().map(|id| (*id, pick(2))))
        .collect();
    assert!(!f.screen.actions.selection.add(&targets));
    snooze_unanswered(&mut f).await;
    feed(&mut f, answer("work", vec![201], Err(not_in_inbox()))).await;
    // Snoozed already, so no archive row.
    feed(
        &mut f,
        answer("work", work[..200].to_vec(), Ok((vec![], 200))),
    )
    .await;
    let failed = Err(OpError::not_found("the daemon said no"));
    feed(&mut f, answer("home", home.clone(), failed)).await;
    let mut back: Vec<i64> = f
        .screen
        .actions
        .selection
        .targets()
        .iter()
        .map(|(id, _)| *id)
        .collect();
    back.sort_unstable();
    assert_eq!(
        back, home,
        "only the failed call's threads; 201 was left out"
    );
}

#[tokio::test]
async fn an_older_returns_failed_call_gives_its_threads_back() {
    let mut f = fixture(2).await;
    let ids = snoozed_selection(&mut f, 2);
    press(&mut f, KeyCode::Char('h')).await;
    let _unrun = f.screen.update(Msg::Key(key(KeyCode::Enter)));
    let older = f.screen.seq;
    let picked = Picked {
        account_id: 1,
        starred: false,
        unread: true,
    };
    let targets: Vec<(i64, Picked)> = ids.iter().map(|id| (*id, picked)).collect();
    assert!(!f.screen.actions.selection.add(&targets));
    press(&mut f, KeyCode::Char('h')).await;
    let _unrun = f.screen.update(Msg::Key(key(KeyCode::Enter)));
    let failed = MailEvent::Unsnoozed {
        seq: older,
        threads: ids.clone(),
        result: Err(OpError::not_found("the daemon said no")),
    };
    feed(&mut f, failed).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Could not return to the inbox: the daemon said no"),
        "{line}"
    );
    assert_eq!(selected_of(&f, 1), ids, "its threads are back");
}
