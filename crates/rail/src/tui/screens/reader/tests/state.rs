//! The reader's cursor across re-reads.

use crossterm::event::KeyCode;
use rail_core::config::ThreadOrder;
use rail_core::ops::SyncPhase;
use rail_core::store::Store;
use rail_core::sync::Batch;
use ratatui::style::Color;

use super::{Fixture, fixture, press, store_bodies_on_fetch};
use crate::tui::app::{Msg, TaskResult};
use crate::tui::data::{DaemonEvent, MailEvent};
use crate::tui::screen::Screen;
use crate::tui::test_util::{BASE_MS, settle, status};
use crate::tui::theme::Theme;

/// Delete message `id` as a sync would, and let the screen re-read.
async fn delete(f: &mut Fixture, id: &str) {
    Store::open(&f.paths.db_file())
        .unwrap()
        .apply(
            &Batch {
                deletes: vec![id.into()],
                ..Batch::new(1)
            },
            BASE_MS / 1000,
        )
        .unwrap();
    let progress = status("work", "w@example.com", SyncPhase::Delta);
    let event = MailEvent::Daemon(DaemonEvent::Progress(Box::new(progress)));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
}

#[tokio::test]
async fn a_reread_keeps_the_cursor_line_or_clamps_it_to_the_last() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('j')).await;
    delete(&mut f, "m3").await;
    assert_eq!(
        f.screen.reader().unwrap().cursor,
        1,
        "line 1 is still there"
    );
    press(&mut f, KeyCode::End).await;
    delete(&mut f, "m2").await;
    // No frame was drawn: the reader works at its fallback width, 80.
    let reader = f.screen.reader().unwrap();
    let last = reader.lines(80).len() - 1;
    assert_eq!(reader.cursor, last, "the thread got shorter");
    assert!(f.screen.reader_cursor_text().is_some());
}

#[tokio::test]
async fn the_reader_gets_its_threads_account_and_the_configured_order() {
    let mut f = fixture(false).await;
    press(&mut f, KeyCode::Enter).await;
    let reader = f.screen.reader().expect("the reader is open");
    let account = reader.account();
    assert_eq!((account.index, account.color), (0, Some(Color::Cyan)));
    assert_eq!(account.badge, "W");
    assert_eq!(account.label, "work");
    assert_eq!(account.color(&Theme::default()), Color::Cyan);
    assert_eq!(reader.order(), ThreadOrder::Oldest);
}
