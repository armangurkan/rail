//! The reader's cursor across re-reads.

use crossterm::event::KeyCode;
use rail_core::ops::SyncPhase;
use rail_core::store::Store;
use rail_core::sync::Batch;

use super::{Fixture, fixture, press, store_bodies_on_fetch};
use crate::tui::app::{Msg, TaskResult};
use crate::tui::data::{DaemonEvent, MailEvent};
use crate::tui::screen::Screen;
use crate::tui::test_util::{BASE_MS, settle, status};

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
