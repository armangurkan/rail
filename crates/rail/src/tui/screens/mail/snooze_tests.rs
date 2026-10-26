//! The snooze chooser, the Snoozed mailbox and the reader's line, drawn at the design sizes
//! (90x24 and 150x40) over the view tests' mailbox, in both icon sets where a glyph is new; and
//! the chooser at sizes too small for it, drawn without panicking.

use crossterm::event::KeyCode;
use rail_core::config::{Icons, TimeFormat};
use rail_core::ops::SyncPhase;
use rail_core::paths::Paths;
use rail_core::store::Store;

use super::MailScreen;
use super::view_tests::{draw, press, screen};
use crate::tui::app::{Msg, TaskResult};
use crate::tui::data::{DaemonEvent, MailEvent};
use crate::tui::screen::Screen;
use crate::tui::test_util::{NOW, settle, status};

/// Mon 13 Oct 2025 08:00 UTC.
const MONDAY_MORNING: i64 = NOW - 45_600 + 3 * 86_400 + 8 * 3_600;

/// Takes the view tests' thread "wt1" (account 1, messages w1-w3) out of the inbox and snoozes it
/// until `until`, as the daemon leaves it once its archive is sent.
fn snooze_wt1(tmp: &tempfile::TempDir, until: i64) {
    let store = Store::open(&Paths::all_in(tmp.path()).db_file()).unwrap();
    let conn = store.conn();
    conn.execute(
        "DELETE FROM message_folders
         WHERE folder_id IN (SELECT id FROM folders WHERE kind = 'inbox')
           AND message_id IN (SELECT id FROM messages WHERE remote_id IN ('w1', 'w2', 'w3'))",
        [],
    )
    .unwrap();
    let insert = format!(
        "INSERT INTO snoozes (account_id, thread_remote, until, created_at)
         VALUES (1, 'wt1', {until}, {NOW})"
    );
    conn.execute(&insert, []).unwrap();
    let snooze = conn.last_insert_rowid();
    for id in ["w1", "w2", "w3"] {
        conn.execute(
            "INSERT INTO snooze_messages (snooze_id, message_remote) VALUES (?1, ?2)",
            (snooze, id),
        )
        .unwrap();
    }
}

/// The store changed, as the daemon reports it (work's status as `screen` set it).
async fn changed(screen: &mut MailScreen) {
    let status = status("work", "w@example.com", SyncPhase::Idle);
    let event = MailEvent::Daemon(DaemonEvent::Progress(Box::new(status)));
    let effects = screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(screen, effects).await;
}

async fn typed(screen: &mut MailScreen, text: &str) {
    for c in text.chars() {
        press(screen, KeyCode::Char(c)).await;
    }
}

async fn open_snoozed(screen: &mut MailScreen) {
    press(screen, KeyCode::Char(':')).await;
    typed(screen, "mailbox snoozed").await;
    press(screen, KeyCode::Enter).await;
}

#[tokio::test]
async fn the_chooser_drawn_with_its_choices_a_typed_time_and_text_it_cannot_read() {
    let (_tmp, mut screen) = screen(Icons::Unicode).await;
    press(&mut screen, KeyCode::Char('h')).await;
    insta::assert_snapshot!("snooze_picker_90x24", draw(&screen, 90, 24));
    insta::assert_snapshot!("snooze_picker_150x40", draw(&screen, 150, 40));
    typed(&mut screen, "6pm").await;
    insta::assert_snapshot!("snooze_picker_typed_90x24", draw(&screen, 90, 24));
    insta::assert_snapshot!("snooze_picker_typed_150x40", draw(&screen, 150, 40));
    for _ in 0..3 {
        press(&mut screen, KeyCode::Backspace).await;
    }
    typed(&mut screen, "soon").await;
    insta::assert_snapshot!("snooze_picker_typed_error_90x24", draw(&screen, 90, 24));
}

#[tokio::test]
async fn the_chooser_drawn_in_twelve_hour_time_and_with_a_last_used_time() {
    let (tmp, mut screen) = screen(Icons::Unicode).await;
    screen.ctx.clock.set_format(TimeFormat::H12);
    press(&mut screen, KeyCode::Char('h')).await;
    insta::assert_snapshot!("snooze_picker_12h_90x24", draw(&screen, 90, 24));
    press(&mut screen, KeyCode::Esc).await;
    screen.ctx.clock.set_format(TimeFormat::H24);
    let state = Paths::all_in(tmp.path()).state_dir;
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("snooze-last"), "2 days").unwrap();
    screen.load_last_snooze();
    press(&mut screen, KeyCode::Char('h')).await;
    insta::assert_snapshot!("snooze_picker_last_used_90x24", draw(&screen, 90, 24));
}

#[tokio::test]
async fn a_snoozed_thread_drawn_in_its_mailbox_its_chooser_and_its_reader() {
    for (icons, name) in [(Icons::Unicode, "unicode"), (Icons::Ascii, "ascii")] {
        let (tmp, mut screen) = screen(icons).await;
        snooze_wt1(&tmp, MONDAY_MORNING);
        changed(&mut screen).await;
        open_snoozed(&mut screen).await;
        let mailbox = draw(&screen, 150, 40);
        insta::assert_snapshot!(format!("mail_snoozed_mailbox_{name}_150x40"), mailbox);
        if icons == Icons::Ascii {
            continue;
        }
        press(&mut screen, KeyCode::Char('h')).await;
        insta::assert_snapshot!("snooze_picker_snoozed_90x24", draw(&screen, 90, 24));
        insta::assert_snapshot!("snooze_picker_snoozed_150x40", draw(&screen, 150, 40));
        press(&mut screen, KeyCode::Esc).await;
        press(&mut screen, KeyCode::Enter).await;
        insta::assert_snapshot!("reader_snoozed_90x24", draw(&screen, 90, 24));
        insta::assert_snapshot!("reader_snoozed_150x40", draw(&screen, 150, 40));
    }
}

#[tokio::test]
async fn the_chooser_draws_at_sizes_too_small_for_it_without_panicking() {
    let (_tmp, mut screen) = screen(Icons::Ascii).await;
    press(&mut screen, KeyCode::Char('h')).await;
    typed(&mut screen, "6pm").await;
    for (width, height) in [(0, 24), (1, 24), (90, 3), (3, 2)] {
        let _ = draw(&screen, width, height);
    }
}
