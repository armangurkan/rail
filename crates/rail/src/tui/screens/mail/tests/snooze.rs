//! `h` and the snooze chooser with the fake daemon over a seeded store: a choice or a typed time
//! snoozes the cursor row, the open thread (closing the reader) or the selection, one
//! `snooze.set` per account with grace; a snoozed thread's chooser offers "Return to inbox now"
//! (`snooze.clear`); Drafts refuse it; the notice and `z`; `snooze.woke` reads the list again.

use rail_core::ops::{OpError, SnoozeSetResponse, SnoozeWoke};

use super::compose::type_text;
use super::drafts_folder::{drafts_fixture, open_drafts};
use super::*;
use crate::tui::test_util::{NOW, render_lines};

/// Friday 2025-10-10 00:00 UTC, the day of `NOW` (12:40).
const MIDNIGHT: i64 = NOW - 45_600;
const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;

/// The fake's `snooze.set` and `snooze.clear` calls, in order.
fn snoozes(f: &Fixture) -> Vec<FakeCall> {
    f.fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| matches!(c, FakeCall::SnoozeSet { .. } | FakeCall::SnoozeClear { .. }))
        .cloned()
        .collect()
}

/// A `snooze.set` of work's `threads` until `until`, with grace.
fn set(threads: Vec<i64>, until: i64) -> FakeCall {
    FakeCall::SnoozeSet {
        account: "work".into(),
        threads,
        until,
        grace: true,
    }
}

/// The screen at 90x24.
fn drawn(f: &Fixture) -> String {
    render_lines(90, 24, |frame| f.screen.view(frame, &Theme::default())).join("\n")
}

/// The row `needle` is drawn on.
fn line_of(shown: &str, needle: &str) -> usize {
    shown
        .lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not drawn:\n{shown}"))
}

fn cursor_thread(f: &Fixture) -> i64 {
    f.screen.list.selected().unwrap().key.thread_id
}

/// An active snooze of work's `thread` until `until`, as the daemon records one once its archive
/// was sent. The thread stays in the inbox here, so the list still shows it.
fn snoozed_in_store(f: &Fixture, thread: i64, until: i64) {
    let store = writer(f);
    let remote: String = store
        .conn()
        .query_row(
            "SELECT remote_id FROM threads WHERE id = ?1",
            [thread],
            |r| r.get(0),
        )
        .unwrap();
    let sql = format!(
        "INSERT INTO snoozes (account_id, thread_remote, until, created_at) VALUES (1, ?1, {until}, {NOW})"
    );
    store.conn().execute(&sql, [remote]).unwrap();
}

/// Take `thread`'s messages out of their inbox, or put them back (`back`), as a snooze and its
/// wake leave them, without telling the screen.
fn in_inbox(f: &Fixture, thread: i64, back: bool) {
    let sql = if back {
        "INSERT INTO message_folders (message_id, folder_id)
         SELECT m.id, fo.id FROM messages m
         JOIN folders fo ON fo.account_id = m.account_id AND fo.kind = 'inbox'
         WHERE m.thread_id = ?1"
    } else {
        "DELETE FROM message_folders
         WHERE message_id IN (SELECT id FROM messages WHERE thread_id = ?1)
           AND folder_id IN (SELECT id FROM folders WHERE kind = 'inbox')"
    };
    writer(f).conn().execute(sql, [thread]).unwrap();
}

/// `snooze.woke` for work's `threads`, as the daemon link reports it.
async fn woke(f: &mut Fixture, threads: Vec<i64>) {
    let woke = SnoozeWoke {
        account: "work".into(),
        threads,
    };
    let event = MailEvent::Daemon(DaemonEvent::Woke(woke));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
}

#[tokio::test]
async fn h_offers_the_times_and_enter_snoozes_the_cursor_row_with_grace() {
    let mut f = fixture(3).await;
    let top = cursor_thread(&f);
    press(&mut f, KeyCode::Char('h')).await;
    let shown = drawn(&f);
    for (label, time) in [
        ("Later today", "Fri 18:00"),
        ("Tomorrow", "Sat 08:00"),
        ("Next week", "Mon 08:00"),
    ] {
        assert!(
            shown.lines().any(|l| l.contains(label) && l.contains(time)),
            "{label} at {time}:\n{shown}"
        );
    }
    assert!(shown.contains("Snooze until"), "{shown}");
    press(&mut f, KeyCode::Down).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(f.screen.modal.is_none(), "the chooser closes");
    assert_eq!(snoozes(&f), [set(vec![top], MIDNIGHT + DAY + 8 * HOUR)]);
    assert!(applies(&f).is_empty(), "a snooze is not an action.apply");
}

#[tokio::test]
async fn esc_closes_the_chooser_and_sends_nothing() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('h')).await;
    assert!(f.screen.modal.is_some());
    press(&mut f, KeyCode::Esc).await;
    assert!(f.screen.modal.is_none());
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(snoozes(&f), []);
}

#[tokio::test]
async fn h_in_the_reader_snoozes_the_open_thread_and_closes_the_reader() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Enter).await;
    let open = f.screen.reader().unwrap().thread_id();
    press(&mut f, KeyCode::Char('h')).await;
    press(&mut f, KeyCode::Enter).await;
    assert!(f.screen.reader().is_none(), "snoozing closes the reader");
    assert_eq!(snoozes(&f), [set(vec![open], MIDNIGHT + 18 * HOUR)]);
}

#[tokio::test]
async fn a_selection_is_snoozed_with_one_call_per_account() {
    let mut f = fixture(2).await;
    let all: BTreeSet<i64> = thread_ids(&f).into_iter().collect();
    press_n(&mut f, KeyCode::Char(' '), all.len()).await;
    press(&mut f, KeyCode::Char('h')).await;
    press(&mut f, KeyCode::Enter).await;
    let mut accounts = Vec::new();
    let mut snoozed = BTreeSet::new();
    for call in snoozes(&f) {
        let FakeCall::SnoozeSet {
            account,
            threads,
            until,
            grace,
        } = call
        else {
            panic!("{call:?}");
        };
        assert_eq!((until, grace), (MIDNIGHT + 18 * HOUR, true));
        assert_eq!(threads.len(), 2, "{account}");
        accounts.push(account);
        snoozed.extend(threads);
    }
    assert_eq!(accounts, ["work", "home"]);
    assert_eq!(snoozed, all);
    assert!(
        f.screen.actions.selection.is_empty(),
        "the selection is used"
    );
}

#[tokio::test]
async fn a_typed_time_comes_first_and_enter_snoozes_until_it() {
    let mut f = fixture(3).await;
    let top = cursor_thread(&f);
    press(&mut f, KeyCode::Char('h')).await;
    type_text(&mut f, "7pm").await;
    let shown = drawn(&f);
    let typed = line_of(&shown, "Fri 19:00");
    assert!(shown.lines().nth(typed).unwrap().contains("At "), "{shown}");
    assert!(!shown.contains("Later today"), "the choices are filtered");
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(snoozes(&f), [set(vec![top], MIDNIGHT + 19 * HOUR)]);
}

#[tokio::test]
async fn text_that_is_not_a_time_says_why_and_enter_does_nothing() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('h')).await;
    type_text(&mut f, "soon").await;
    let shown = drawn(&f);
    assert!(
        shown.contains("Try 18:00, 6pm, 2026-11-03 09:30, +3h, +2d or mon."),
        "{shown}"
    );
    press(&mut f, KeyCode::Enter).await;
    assert!(f.screen.modal.is_some(), "nothing to choose");
    for _ in "soon".chars() {
        press(&mut f, KeyCode::Backspace).await;
    }
    type_text(&mut f, "2025-10-01").await;
    assert!(drawn(&f).contains("That time has passed."));
    assert_eq!(snoozes(&f), []);
}

#[tokio::test]
async fn a_snoozed_thread_is_offered_return_to_inbox_now_first() {
    let mut f = fixture(3).await;
    let top = cursor_thread(&f);
    snoozed_in_store(&f, top, MIDNIGHT + 3 * DAY + 8 * HOUR);
    press(&mut f, KeyCode::Char('h')).await;
    let shown = drawn(&f);
    assert!(
        shown.contains("Snoozed until Mon 13 Oct 2025 08:00"),
        "{shown}"
    );
    assert!(line_of(&shown, "Return to inbox now") < line_of(&shown, "Later today"));
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        snoozes(&f),
        [FakeCall::SnoozeClear {
            account: "work".into(),
            threads: vec![top]
        }]
    );
    let line = f.screen.status_line();
    assert!(line.contains("returned 1 to the inbox"), "{line}");
}

#[tokio::test]
async fn h_is_not_available_on_drafts() {
    let mut f = drafts_fixture().await;
    open_drafts(&mut f).await;
    let before = f.fake.calls.lock().unwrap().len();
    press(&mut f, KeyCode::Char('h')).await;
    let line = f.screen.status_line();
    assert!(line.contains("Not available on drafts"), "{line}");
    assert!(f.screen.modal.is_none(), "no chooser");
    assert_eq!(f.fake.calls.lock().unwrap().len(), before, "no call");
}

#[tokio::test]
async fn the_notice_names_the_time_and_z_undoes_the_snooze() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('h')).await;
    press(&mut f, KeyCode::Enter).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Snoozed until Fri 18:00 · z undoes"),
        "{line}"
    );
    press(&mut f, KeyCode::Char('z')).await;
    let undone: Vec<i64> = f
        .fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            FakeCall::Undo { rows } => Some(rows.iter().map(|r| r.row).collect::<Vec<_>>()),
            _ => None,
        })
        .flatten()
        .collect();
    assert_eq!(undone, [1], "the snooze's archive row");
}

#[tokio::test]
async fn a_snooze_that_queued_no_archive_has_no_undo_hint() {
    let mut f = fixture(3).await;
    *f.fake.snooze_answer.lock().unwrap() = Some(Ok(SnoozeSetResponse {
        rows: vec![],
        snoozed: 1,
    }));
    press(&mut f, KeyCode::Char('h')).await;
    press(&mut f, KeyCode::Enter).await;
    let line = f.screen.status_line();
    assert!(line.contains("Snoozed until Fri 18:00"), "{line}");
    assert!(!line.contains("undoes"), "{line}");
}

#[tokio::test]
async fn a_refused_snooze_says_why_in_its_own_words() {
    let mut f = fixture(3).await;
    *f.fake.snooze_answer.lock().unwrap() = Some(Err(OpError::invalid_input(
        "Only a thread in the inbox can be snoozed.",
    )));
    press(&mut f, KeyCode::Char('h')).await;
    press(&mut f, KeyCode::Enter).await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Could not snooze: Only a thread in the inbox can be snoozed."),
        "{line}"
    );
}

#[tokio::test]
async fn a_wake_reads_the_list_again_and_names_the_thread() {
    let mut f = fixture(3).await;
    let top = cursor_thread(&f);
    let subject = selected(&f).unwrap();
    in_inbox(&f, top, false);
    store_changed(&mut f).await;
    assert!(!thread_ids(&f).contains(&top), "snoozed away");
    in_inbox(&f, top, true);
    woke(&mut f, vec![top]).await;
    assert!(thread_ids(&f).contains(&top), "the list was read again");
    let line = f.screen.status_line();
    assert!(
        line.contains(&format!("Back from snooze: {subject}")),
        "{line}"
    );
}

#[tokio::test]
async fn several_threads_back_are_counted() {
    let mut f = fixture(3).await;
    let threads: Vec<i64> = thread_ids(&f).into_iter().take(3).collect();
    woke(&mut f, threads).await;
    let line = f.screen.status_line();
    assert!(line.contains("3 threads back from snooze"), "{line}");
}
