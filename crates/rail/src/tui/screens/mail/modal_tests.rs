//! The mail screen with its modals: filtering as you type (debounced, stale answers dropped),
//! the palette's targets, help in each view, `:errors`, and `q` typed rather than quit.

use std::sync::Arc;

use crossterm::event::KeyCode;
use rail_core::ops::{OpError, SyncPhase};
use rail_core::paths::Paths;

use super::*;
use crate::tui::app::{App, Msg, TaskResult, update};
use crate::tui::data::{DaemonEvent, FakeDaemon, MailEvent, Timer};
use crate::tui::modals::Target;
use crate::tui::screen::Screen;
use crate::tui::test_util::{
    RecordingClipboard, inbox_threads, key, mail_ctx, seed_store, settle, snapshot, status,
    work_and_home,
};
use crate::tui::theme::Theme;

async fn screen() -> (tempfile::TempDir, MailScreen) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let envelopes: Vec<_> = inbox_threads("w", 10).into_iter().map(|e| (0, e)).collect();
    seed_store(
        &paths,
        &[("work", "w@example.com"), ("home", "h@example.com")],
        &envelopes,
    );
    let fake = Arc::new(FakeDaemon::default());
    *fake.statuses.lock().unwrap() = vec![status("work", "w@example.com", SyncPhase::Idle)];
    let mut screen = MailScreen::new(mail_ctx(&paths, fake), work_and_home());
    let effects = screen.start();
    settle(&mut screen, effects).await;
    (tmp, screen)
}

fn press(screen: &mut MailScreen, code: KeyCode) -> Vec<Effect> {
    screen.update(Msg::Key(key(code)))
}

fn type_in(screen: &mut MailScreen, text: &str) -> Vec<Effect> {
    text.chars()
        .flat_map(|c| press(screen, KeyCode::Char(c)))
        .collect()
}

/// Fire the newest filter timer by hand; the list read it starts (not run yet).
fn fire_filter(screen: &mut MailScreen) -> Vec<Effect> {
    let event = MailEvent::Timer {
        token: screen.filter_token,
        kind: Timer::Filter,
    };
    screen.update(Msg::Task(TaskResult::Mail(event)))
}

fn subjects(screen: &MailScreen) -> Vec<String> {
    screen
        .list
        .rows()
        .iter()
        .map(|r| r.subject.clone())
        .collect()
}

#[tokio::test]
async fn typing_is_debounced_and_only_the_last_pause_reads() {
    let (_tmp, mut screen) = screen().await;
    press(&mut screen, KeyCode::Char('/'));
    let timers = type_in(&mut screen, "wm3");
    assert_eq!(timers.len(), 3, "one timer per keystroke");
    let stale = MailEvent::Timer {
        token: screen.filter_token - 1,
        kind: Timer::Filter,
    };
    assert!(
        screen.update(Msg::Task(TaskResult::Mail(stale))).is_empty(),
        "an older pause reads nothing"
    );
    let reads = fire_filter(&mut screen);
    assert_eq!(reads.len(), 1);
    settle(&mut screen, reads).await;
    assert_eq!(subjects(&screen), ["subject wm3"]);
}

/// Review focus 4.
#[tokio::test]
async fn a_stale_list_result_is_dropped() {
    let (_tmp, mut screen) = screen().await;
    press(&mut screen, KeyCode::Char('/'));
    type_in(&mut screen, "wm3");
    let narrow = fire_filter(&mut screen);
    for _ in 0..3 {
        press(&mut screen, KeyCode::Backspace);
    }
    type_in(&mut screen, "subject");
    let wide = fire_filter(&mut screen);
    settle(&mut screen, wide).await;
    assert_eq!(screen.list.rows().len(), 10);
    settle(&mut screen, narrow).await;
    assert_eq!(
        screen.list.rows().len(),
        10,
        "the older answer arrived last and was dropped"
    );
}

#[tokio::test]
async fn the_status_line_shows_the_parsed_filter_and_esc_clears_it() {
    let (_tmp, mut screen) = screen().await;
    press(&mut screen, KeyCode::Char('/'));
    type_in(&mut screen, "is:unread form:dana");
    let text = snapshot(90, 24, |frame| screen.view(frame, &Theme::default()));
    assert!(text.contains("/is:unread form:dana"), "{text}");
    assert!(
        text.contains("is:unread · words: form:dana"),
        "the typo shows as a word: {text}"
    );
    let effects = fire_filter(&mut screen);
    settle(&mut screen, effects).await;
    press(&mut screen, KeyCode::Enter);
    assert_eq!(screen.slot_text(), "/ is:unread · words: form:dana");
    assert!(screen.list.rows().is_empty());
    press(&mut screen, KeyCode::Char('/'));
    let effects = press(&mut screen, KeyCode::Esc);
    settle(&mut screen, effects).await;
    assert!(screen.filter.is_empty());
    assert_eq!(screen.list.rows().len(), 10);
    assert!(!screen.status_line().starts_with("filter:"));
}

#[tokio::test]
async fn q_is_typed_in_the_filter_line_and_the_palette() {
    let (_tmp, screen) = screen().await;
    let mut app = App::new(screen, Box::new(RecordingClipboard::default()));
    update(&mut app, Msg::Key(key(KeyCode::Char('/'))));
    update(&mut app, Msg::Key(key(KeyCode::Char('q'))));
    assert!(!app.quit);
    assert_eq!(app.screen.filter_text, "q");
    update(&mut app, Msg::Key(key(KeyCode::Esc)));
    update(&mut app, Msg::Key(key(KeyCode::Char(':'))));
    update(&mut app, Msg::Key(key(KeyCode::Char('q'))));
    assert!(!app.quit);
    update(&mut app, Msg::Key(key(KeyCode::Esc)));
    update(&mut app, Msg::Key(key(KeyCode::Char('q'))));
    assert!(app.quit, "outside a modal, q quits from the list");
}

#[tokio::test]
async fn the_palette_goes_to_accounts_and_labels_and_shows_bound_keys() {
    let (_tmp, mut screen) = screen().await;
    let entries = screen.palette_entries();
    let filter = entries
        .iter()
        .find(|e| e.target == Target::Action(crate::tui::keys::Action::ListFilter))
        .unwrap();
    assert_eq!(filter.key, "/");
    let home = entries
        .iter()
        .find(|e| e.text == "Go to home's inbox")
        .unwrap();
    assert_eq!(home.key, "2");
    assert!(
        entries.iter().any(|e| e.text == "Label: work · Projects"),
        "{entries:?}"
    );
    press(&mut screen, KeyCode::Char(':'));
    let effects = type_in(&mut screen, "inbox home");
    assert!(effects.is_empty(), "the palette touches no store");
    let effects = press(&mut screen, KeyCode::Enter);
    settle(&mut screen, effects).await;
    assert_eq!(screen.scope, ScopeChoice::Account(1));
}

#[tokio::test]
async fn colon_errors_lists_what_went_wrong() {
    let (_tmp, mut screen) = screen().await;
    let lost = MailEvent::Daemon(DaemonEvent::Lost(OpError::internal(
        "the daemon \u{1b}[2J went away",
    )));
    screen.update(Msg::Task(TaskResult::Mail(lost)));
    press(&mut screen, KeyCode::Char(':'));
    type_in(&mut screen, "errors");
    press(&mut screen, KeyCode::Enter);
    let text = snapshot(90, 24, |frame| screen.view(frame, &Theme::default()));
    assert!(text.contains("the daemon \\u{1b}[2J went away"), "{text}");
    assert!(!text.contains('\u{1b}'));
}

#[tokio::test]
async fn help_shows_the_keys_of_the_view_you_are_in() {
    let (_tmp, mut screen) = screen().await;
    press(&mut screen, KeyCode::Char('?'));
    let text = snapshot(90, 40, |frame| screen.view(frame, &Theme::default()));
    assert!(
        text.contains("Thread list") && text.contains("Filter the list as you type"),
        "{text}"
    );
    press(&mut screen, KeyCode::Esc);
    let effects = press(&mut screen, KeyCode::Enter);
    settle(&mut screen, effects).await;
    press(&mut screen, KeyCode::Char('?'));
    let text = snapshot(90, 40, |frame| screen.view(frame, &Theme::default()));
    assert!(
        text.contains("Reader") && text.contains("Open a link"),
        "{text}"
    );
}

#[tokio::test]
async fn a_palette_action_picked_in_the_reader_keeps_the_reader_open() {
    let (_tmp, mut screen) = screen().await;
    let effects = press(&mut screen, KeyCode::Enter);
    settle(&mut screen, effects).await;
    assert!(screen.reader().is_some());
    press(&mut screen, KeyCode::Char(':'));
    type_in(&mut screen, "keys for this view");
    press(&mut screen, KeyCode::Enter);
    assert!(screen.reader().is_some(), "the reader stays open");
    let text = snapshot(90, 40, |frame| screen.view(frame, &Theme::default()));
    assert!(
        text.contains("Reader") && text.contains("Open a link"),
        "{text}"
    );
}

#[tokio::test]
async fn a_list_action_from_the_palette_works_from_the_sidebar() {
    let (_tmp, mut screen) = screen().await;
    press(&mut screen, KeyCode::Tab);
    assert_eq!(screen.focus, Focus::Sidebar);
    screen.run_palette(Target::Action(crate::tui::keys::Action::ListFilter));
    type_in(&mut screen, "x");
    assert_eq!(screen.filter_text, "x", "the filter line opened");
    assert_eq!(screen.focus, Focus::List);
}

#[tokio::test]
async fn the_palette_lists_the_keys_of_the_active_view() {
    use crate::tui::keys::Action;
    let (_tmp, mut screen) = screen().await;
    let has = |screen: &MailScreen, a: Action| {
        screen
            .palette_entries()
            .iter()
            .any(|e| e.target == Target::Action(a))
    };
    assert!(has(&screen, Action::ListFilter) && has(&screen, Action::GlobalHelp));
    assert!(!has(&screen, Action::ReaderFollowLink));
    let effects = press(&mut screen, KeyCode::Enter);
    settle(&mut screen, effects).await;
    assert!(has(&screen, Action::ReaderFollowLink) && has(&screen, Action::GlobalHelp));
    assert!(!has(&screen, Action::ListFilter));
}
