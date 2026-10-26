//! The reader's keys and commands through the mail screen: `h` (and `?` listing it), `e` on
//! a card, and `:thread-order` with its save and its failed save.

use crossterm::event::KeyCode;
use rail_core::config::ThreadOrder;
use rail_core::store::Store;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;

use super::{
    Fixture, all_text, draw, fixture, keys, open_toms_card, press, store_bodies_on_fetch,
    write_bodies,
};
use crate::tui::data::FakeCall;
use crate::tui::modals::Target;
use crate::tui::screen::Screen;
use crate::tui::screens::reader::LineKind;
use crate::tui::theme::Theme;

#[tokio::test]
async fn shift_h_shows_and_hides_the_headers_and_help_lists_it() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    Store::open(&f.paths.db_file())
        .unwrap()
        .conn()
        .execute(
            "UPDATE messages SET cc_json = '[{\"name\":\"Priya Nair\",\"address\":\"priya@example.com\"}]', \
             subject = 'Re: Q4 plan' WHERE remote_id = 'm1'",
            [],
        )
        .unwrap();
    // 12-hour times, as `:time` sets them.
    press(&mut f, KeyCode::Char(':')).await;
    keys(&mut f, "time").await;
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Enter).await;
    open_toms_card(&mut f).await;
    assert!(!all_text(&f).contains("From:"), "hidden at first");
    keys(&mut f, "H").await;
    let text = all_text(&f);
    assert!(text.contains("From: Tom <tom@example.com>"), "{text}");
    assert!(text.contains("To: Dana Reyes <dana@example.com>"), "{text}");
    assert!(
        text.contains("Cc: Priya Nair <priya@example.com>"),
        "{text}"
    );
    assert!(text.contains("Date: Thu 09 Oct 2025 8:53 am"), "{text}");
    assert!(
        text.contains("Subject: Q4 plan"),
        "the newest's subject differs from the thread's (now Tom's): {text}"
    );
    assert!(
        text.contains("From: Sender <sender@example.com>"),
        "the newest's too: {text}"
    );
    assert!(
        f.screen
            .reader_cursor_text()
            .unwrap()
            .starts_with("▾   1   Tom"),
        "the cursor stays on Tom's card"
    );
    keys(&mut f, "H").await;
    assert!(!all_text(&f).contains("From:"), "{}", all_text(&f));
    keys(&mut f, "?").await;
    let help = draw(&f, 150, 40);
    assert!(help.contains("Show or hide the headers"), "{help}");
    assert!(
        help.contains("Show the quoted lines, or open the message"),
        "Enter's row says it opens a card too: {help}"
    );
}

/// Whether the first cell of `name` on the drawn row holding `card` is bold.
fn drawn_bold(f: &Fixture, card: &str, name: &str) -> bool {
    let (width, height) = (120, 40);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| f.screen.view(frame, &Theme::default()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    for y in 0..height {
        let row: Vec<&str> = (0..width).map(|x| buffer[(x, y)].symbol()).collect();
        let text = row.concat();
        if let Some(at) = text
            .find(card)
            .and_then(|c| text[c..].find(name).map(|n| c + n))
        {
            let x = u16::try_from(text[..at].chars().count()).unwrap();
            return buffer[(x, y)].modifier.contains(Modifier::BOLD);
        }
    }
    panic!("{card:?} is not drawn");
}

/// Spec §2.3: a card's sender is bold while the message was unread when the thread opened,
/// though opening marks it read and the re-read after the bodies land sees it read.
#[tokio::test]
async fn an_older_message_unread_at_open_draws_its_sender_bold_after_the_reread() {
    let mut f = fixture(false).await;
    let set_read = |paths: &rail_core::paths::Paths, sql: &str| {
        let store = Store::open(&paths.db_file()).unwrap();
        store.conn().execute(sql, []).unwrap();
    };
    set_read(
        &f.paths,
        "UPDATE messages SET is_read = 0 WHERE remote_id = 'm1'",
    );
    // The daemon has marked the thread read by the time it stores the bodies.
    let paths = f.paths.clone();
    *f.fake.on_fetch.lock().unwrap() = Some(Box::new(move |ids: &[String]| {
        set_read(&paths, "UPDATE messages SET is_read = 1");
        write_bodies(&paths, ids);
    }));
    press(&mut f, KeyCode::Enter).await;
    assert!(
        all_text(&f).contains("body of m3"),
        "the re-read after the fetch settled: {}",
        all_text(&f)
    );
    assert!(
        drawn_bold(&f, "1   Tom", "Tom"),
        "Tom's card was unread at open"
    );
    assert!(!drawn_bold(&f, "2   Sender", "Sender"), "m2 was read");
}

/// Write `config.toml` as the user would have it.
fn write_config(f: &Fixture, text: &str) {
    let path = f.paths.config_file();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn config_text(f: &Fixture) -> String {
    std::fs::read_to_string(f.paths.config_file()).unwrap_or_default()
}

/// `:thread-order` typed in the palette and run with Enter.
async fn run_thread_order(f: &mut Fixture) {
    press(f, KeyCode::Char(':')).await;
    keys(f, "thread-order").await;
    press(f, KeyCode::Enter).await;
}

fn has_entry(f: &Fixture, text: &str) -> bool {
    f.screen
        .palette_entries()
        .iter()
        .any(|e| e.target == Target::ThreadOrder && e.text == text)
}

/// Where Tom's card (m1, number 1) and Sender's (m2, number 2) start in the reader's text.
fn card_places(f: &Fixture) -> (usize, usize) {
    let text = all_text(f);
    (
        text.find("   1   Tom").expect("Tom's card"),
        text.find("   2   Sender").expect("Sender's card"),
    )
}

#[tokio::test]
async fn e_on_a_card_archives_the_thread_and_closes_the_reader() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    let thread = f.screen.reader().unwrap().thread_id();
    keys(&mut f, "n").await;
    assert_eq!(
        f.screen.reader().unwrap().cursor_line().unwrap().kind,
        LineKind::Card { message: 0 }
    );
    keys(&mut f, "e").await;
    assert!(f.screen.reader().is_none(), "archive closes the reader");
    let calls = f.fake.calls.lock().unwrap();
    assert!(
        calls.iter().any(|c| matches!(
            c,
            FakeCall::Apply { threads, action: rail_core::ops::Action::Archive {}, .. }
                if *threads == [thread]
        )),
        "{calls:?}"
    );
}

#[tokio::test]
async fn colon_thread_order_turns_the_open_readers_cards_round_and_saves_the_choice() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    write_config(&f, "[general]\neditor = \"vi\" # mine\n");
    press(&mut f, KeyCode::Enter).await;
    keys(&mut f, "n").await;
    assert!(has_entry(&f, "Show older messages newest first"));
    let (tom, sender) = card_places(&f);
    assert!(tom < sender, "oldest first");

    run_thread_order(&mut f).await;
    assert!(
        f.screen.status_line().contains("thread order: newest"),
        "{}",
        f.screen.status_line()
    );
    let (tom, sender) = card_places(&f);
    assert!(sender < tom, "the open reader runs newest first");
    assert!(
        f.screen
            .reader_cursor_text()
            .unwrap()
            .starts_with("▸   1   Tom"),
        "the cursor stays on Tom's card"
    );
    let text = config_text(&f);
    assert!(text.contains("thread_order = \"newest\""), "{text}");
    assert!(text.contains("editor = \"vi\" # mine"), "{text}");
    let config = rail_core::config::parse(&text, &f.paths.config_file()).unwrap();
    assert_eq!(config.general.thread_order, ThreadOrder::Newest);
    assert!(has_entry(&f, "Show older messages oldest first"));

    keys(&mut f, "q").await;
    press(&mut f, KeyCode::Enter).await;
    let (tom, sender) = card_places(&f);
    assert!(sender < tom, "a thread opened now runs newest first too");

    run_thread_order(&mut f).await;
    assert!(
        f.screen.status_line().contains("thread order: oldest"),
        "{}",
        f.screen.status_line()
    );
    let (tom, sender) = card_places(&f);
    assert!(tom < sender);
    let text = config_text(&f);
    assert!(
        !text.contains("thread_order"),
        "oldest first is the default: {text}"
    );
    assert!(text.contains("editor = \"vi\" # mine"), "{text}");
}

#[tokio::test]
async fn a_failed_thread_order_save_keeps_the_order_and_reports() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    crate::tui::test_util::block_config(&f.paths);
    let errors = f.screen.error_count();
    run_thread_order(&mut f).await;
    assert!(
        f.screen
            .status_line()
            .contains("Could not save the thread order"),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(f.screen.error_count(), errors + 1);
    assert!(
        has_entry(&f, "Show older messages oldest first"),
        "newest first for the session"
    );
    let (tom, sender) = card_places(&f);
    assert!(sender < tom);
}
