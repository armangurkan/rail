//! `h` and the snooze chooser with the fake daemon over a seeded store: a choice or a typed time
//! snoozes the cursor row, the open thread (closing the reader) or the selection, one
//! `snooze.set` per account with grace; a snoozed thread's chooser offers "Return to inbox now"
//! (`snooze.clear`); Drafts refuse it; the notice and `z`; `snooze.woke` reads the list again.

use rail_core::ops::{OpError, SnoozeSetResponse, SnoozeWoke};

use super::compose::type_text;
use super::drafts_folder::{drafts_fixture, open_drafts};
use super::*;
use crate::tui::screens::mail::snooze_flow::read_last;
use crate::tui::snooze::SOON;
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
        shown.contains("Try 6pm, tomorrow 9am, 2 days, nov 3 or fri."),
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

/// On a snoozed thread, `noon` typed, then `keys`, then Enter: what was sent, and the thread.
async fn noon_then(keys: &[KeyCode]) -> (Vec<FakeCall>, i64) {
    let mut f = fixture(3).await;
    let top = cursor_thread(&f);
    snoozed_in_store(&f, top, MIDNIGHT + 3 * DAY + 8 * HOUR);
    press(&mut f, KeyCode::Char('h')).await;
    type_text(&mut f, "noon").await;
    for key in keys {
        press(&mut f, *key).await;
    }
    press(&mut f, KeyCode::Enter).await;
    (snoozes(&f), top)
}

#[tokio::test]
async fn up_and_down_move_between_a_typed_time_and_return_to_inbox_now() {
    // `noon` reads as Sat 12:00 and matches "Return to inbox now": the typed row comes first.
    let noon = MIDNIGHT + DAY + 12 * HOUR;
    for (keys, back) in [
        (&[][..], false),
        (&[KeyCode::Down][..], true),
        (&[KeyCode::Down, KeyCode::Down][..], true),
        (&[KeyCode::Down, KeyCode::Up][..], false),
        (&[KeyCode::Up][..], false),
    ] {
        let (sent, top) = noon_then(keys).await;
        let want = if back {
            FakeCall::SnoozeClear {
                account: "work".into(),
                threads: vec![top],
            }
        } else {
            set(vec![top], noon)
        };
        assert_eq!(sent, [want], "{keys:?}");
    }
}

#[tokio::test]
async fn returning_a_selection_to_the_inbox_uses_up_the_selection() {
    let mut f = fixture(3).await;
    let top = cursor_thread(&f);
    snoozed_in_store(&f, top, MIDNIGHT + 3 * DAY + 8 * HOUR);
    press_n(&mut f, KeyCode::Char(' '), 2).await;
    assert_eq!(f.screen.actions.selection.len(), 2);
    press(&mut f, KeyCode::Char('h')).await;
    press(&mut f, KeyCode::Enter).await;
    let sent = snoozes(&f);
    assert!(
        !sent.is_empty()
            && sent
                .iter()
                .all(|c| matches!(c, FakeCall::SnoozeClear { .. })),
        "{sent:?}"
    );
    assert!(
        f.screen.actions.selection.is_empty(),
        "the selection is used"
    );
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

#[tokio::test]
async fn typing_a_choices_name_offers_that_choice_once() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('h')).await;
    type_text(&mut f, "tomorrow").await;
    let shown = drawn(&f);
    assert_eq!(shown.matches("Sat 08:00").count(), 1, "{shown}");
    press(&mut f, KeyCode::Enter).await;
    let top = f.screen.list.selected().unwrap().key.thread_id;
    assert_eq!(snoozes(&f), [set(vec![top], MIDNIGHT + DAY + 8 * HOUR)]);
}

/// The screen again over the same files and daemon, as Rail starting anew.
async fn restart(f: &mut Fixture) {
    f.screen = MailScreen::new(mail_ctx(&f.paths, f.fake.clone()), work_and_home());
    let effects = f.screen.start();
    settle(&mut f.screen, effects).await;
}

fn last_file(f: &Fixture) -> std::path::PathBuf {
    f.paths.state_dir.join("snooze-last")
}

#[tokio::test]
async fn a_typed_time_is_offered_again_as_last_used_even_after_a_restart() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('h')).await;
    type_text(&mut f, "7pm").await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(std::fs::read_to_string(last_file(&f)).unwrap(), "7pm");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(last_file(&f))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    for screen in ["this session", "after a restart"] {
        if screen == "after a restart" {
            restart(&mut f).await;
        }
        press(&mut f, KeyCode::Char('h')).await;
        let shown = drawn(&f);
        assert!(
            line_of(&shown, "Last used") < line_of(&shown, "Later today"),
            "{screen}: {shown}"
        );
        let row = shown.lines().nth(line_of(&shown, "Last used")).unwrap();
        assert!(row.contains("Fri 19:00"), "{screen}: {row}");
        press(&mut f, KeyCode::Esc).await;
    }
}

#[tokio::test]
async fn last_used_is_left_out_when_a_choice_says_the_same_or_it_has_passed() {
    let mut f = fixture(3).await;
    std::fs::create_dir_all(&f.paths.state_dir).unwrap();
    for (said, why) in [("tomorrow", "the Tomorrow row"), ("2025-10-01", "passed")] {
        std::fs::write(last_file(&f), said).unwrap();
        restart(&mut f).await;
        press(&mut f, KeyCode::Char('h')).await;
        let shown = drawn(&f);
        assert!(!shown.contains("Last used"), "{why}: {shown}");
        press(&mut f, KeyCode::Esc).await;
    }
}

#[tokio::test]
async fn a_last_used_file_padded_past_what_rail_writes_is_ignored() {
    let mut f = fixture(3).await;
    std::fs::create_dir_all(&f.paths.state_dir).unwrap();
    let padded = format!("{}7pm", " ".repeat(64 * 1024));
    std::fs::write(last_file(&f), padded).unwrap();
    restart(&mut f).await;
    press(&mut f, KeyCode::Char('h')).await;
    let shown = drawn(&f);
    assert!(!shown.contains("Last used"), "{shown}");
}

/// A pipe where the file should be: the read never waits for a writer, so Rail still starts.
#[cfg(unix)]
#[test]
fn a_pipe_in_place_of_the_last_used_file_is_not_waited_on() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("snooze-last");
    let made = std::process::Command::new("mkfifo")
        .arg(&path)
        .status()
        .unwrap();
    assert!(made.success(), "mkfifo: {made}");
    let (done, answer) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(read_last(&path)));
    assert_eq!(
        answer.recv_timeout(std::time::Duration::from_secs(5)),
        Ok(None),
        "the read waited on the pipe"
    );
}

#[tokio::test]
async fn a_typed_time_longer_than_rail_keeps_snoozes_but_is_not_kept() {
    let mut f = fixture(3).await;
    let top = cursor_thread(&f);
    press(&mut f, KeyCode::Char('h')).await;
    type_text(&mut f, &format!("tomorrow{}9am", " ".repeat(100))).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(snoozes(&f), [set(vec![top], MIDNIGHT + DAY + 9 * HOUR)]);
    assert!(!last_file(&f).exists(), "kept");
    press(&mut f, KeyCode::Char('h')).await;
    let shown = drawn(&f);
    assert!(!shown.contains("Last used"), "{shown}");
}

/// The open chooser's clock moves on (it keeps the clock it was opened with).
fn chooser_clock(f: &mut Fixture, now: i64) {
    let Some(Modal::Picker(picker)) = f.screen.modal.as_mut() else {
        panic!("no chooser is open");
    };
    picker.set_now(now);
}

#[tokio::test]
async fn a_typed_time_that_is_too_soon_by_enter_is_refused_in_the_chooser() {
    let mut f = fixture(3).await;
    press(&mut f, KeyCode::Char('h')).await;
    type_text(&mut f, "+2m").await;
    // 55 seconds ahead by now: the daemon, which wants a minute, would refuse it.
    chooser_clock(&mut f, NOW + 65);
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(snoozes(&f), []);
    let shown = drawn(&f);
    assert!(shown.contains(SOON), "{shown}");
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(snoozes(&f), [], "a second Enter sends nothing either");
}

/// The screen at 150x40, where the sidebar shows.
fn wide(f: &Fixture) -> String {
    render_lines(150, 40, |frame| f.screen.view(frame, &Theme::default())).join("\n")
}

/// `:` then "mailbox snoozed" and Enter.
async fn open_snoozed(f: &mut Fixture) {
    press(f, KeyCode::Char(':')).await;
    type_text(f, "mailbox snoozed").await;
    press(f, KeyCode::Enter).await;
}

/// The open reader's lines at a width where nothing wraps.
fn reader_text(f: &Fixture) -> String {
    let lines = f.screen.reader().unwrap().lines(400);
    lines
        .iter()
        .map(|l| l.text().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn the_sidebar_lists_snoozed_only_while_something_is_snoozed() {
    let mut f = fixture(3).await;
    assert!(!wide(&f).contains("Snoozed"), "nothing is snoozed");
    let top = cursor_thread(&f);
    snoozed_in_store(&f, top, MIDNIGHT + DAY + 8 * HOUR);
    store_changed(&mut f).await;
    let shown = wide(&f);
    let row = shown
        .lines()
        .find(|l| l.contains("Snoozed"))
        .unwrap_or_default();
    assert!(
        row.contains("◷") && row.contains(" 1"),
        "the icon and the count: {shown}"
    );
}

#[tokio::test]
async fn the_snoozed_mailbox_opens_from_the_palette_and_lists_what_is_snoozed() {
    let mut f = fixture(3).await;
    let top = cursor_thread(&f);
    let entries = f.screen.palette_entries();
    assert!(entries.iter().any(|e| e.text == "Mailbox: Snoozed"));
    open_snoozed(&mut f).await;
    assert!(drawn(&f).contains("Nothing is snoozed."), "{}", drawn(&f));
    snoozed_in_store(&f, top, MIDNIGHT + DAY + 8 * HOUR);
    store_changed(&mut f).await;
    assert_eq!(thread_ids(&f), [top]);
}

#[tokio::test]
async fn the_reader_says_until_when_a_thread_is_snoozed_in_either_clock() {
    let mut f = fixture(3).await;
    let top = cursor_thread(&f);
    snoozed_in_store(&f, top, MIDNIGHT + 3 * DAY + 8 * HOUR);
    press(&mut f, KeyCode::Enter).await;
    let text = reader_text(&f);
    assert!(
        text.contains("Snoozed until Mon 13 Oct 2025 08:00"),
        "{text}"
    );
    press(&mut f, KeyCode::Char('q')).await;
    f.screen
        .ctx
        .clock
        .set_format(rail_core::config::TimeFormat::H12);
    press(&mut f, KeyCode::Enter).await;
    let text = reader_text(&f);
    assert!(
        text.contains("Snoozed until Mon 13 Oct 2025 8:00 am"),
        "{text}"
    );
}
