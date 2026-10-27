//! The copies `attachment.open` wrote: kept for `OPENED_GRACE` after the reader closes, so a
//! viewer that starts slowly still finds its file (#108), and removed at once when the screen
//! goes (quitting). A child module of `tests/attachments.rs`, whose helpers it uses.

use std::path::Path;

use crate::tui::open::OPENED_GRACE_TICKS;

use super::*;

/// The paths the recording opener was handed, in order.
fn opened_paths(opened: &Arc<Mutex<Vec<Url>>>) -> Vec<PathBuf> {
    opened
        .lock()
        .unwrap()
        .iter()
        .map(|url| url.to_file_path().expect("a file URL"))
        .collect()
}

/// `n` ticks of the event loop.
fn tick(f: &mut Fixture, n: usize) {
    for _ in 0..n {
        assert!(
            f.screen.update(Msg::Tick).is_empty(),
            "a tick sends nothing"
        );
    }
}

/// Every copy and its token directory are still there.
fn all_there(written: &[PathBuf]) {
    for path in written {
        assert!(path.is_file(), "the copy went early: {path:?}");
    }
}

/// Every copy and its token directory are gone.
fn all_gone(written: &[PathBuf]) {
    for path in written {
        assert!(!path.exists(), "the file stayed: {path:?}");
        let dir = path.parent().map(Path::to_path_buf).unwrap_or_default();
        assert!(!dir.exists(), "the token directory stayed: {dir:?}");
    }
}

/// AC-28, #108: two copies (the first answers the session's question, the second goes straight
/// through; a token directory each) outlive the reader's close for a minute of ticks, then go
/// with their directories.
#[tokio::test]
async fn closing_the_reader_removes_what_it_opened_a_minute_later() {
    let (mut f, opened) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('y')).await;
    press(&mut f, KeyCode::Char('a')).await;
    highlight(&mut f, "logo.png").await;
    press(&mut f, KeyCode::Char('o')).await;
    let written = opened_paths(&opened);
    assert_eq!(written.len(), 2, "{written:?}");
    assert_ne!(written[0].parent(), written[1].parent(), "{written:?}");
    all_there(&written);
    press(&mut f, KeyCode::Char('q')).await;
    assert!(f.screen.reader.is_none(), "the reader closed");
    all_there(&written);
    tick(&mut f, OPENED_GRACE_TICKS - 1);
    all_there(&written);
    tick(&mut f, 1);
    all_gone(&written);
}

/// Quitting does not wait out the grace: dropping the screen removes a closed reader's copies
/// and an open reader's at once.
#[tokio::test]
async fn quitting_removes_every_copy_at_once() {
    let (mut f, opened) = confirmed_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Char('o')).await;
    press(&mut f, KeyCode::Char('q')).await;
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Char('o')).await;
    let written = opened_paths(&opened);
    assert_eq!(written.len(), 2, "one closed, one open: {written:?}");
    tick(&mut f, 1);
    all_there(&written);
    let Fixture { _tmp, screen, .. } = f;
    drop(screen);
    all_gone(&written);
}

/// A reader opened again within the grace keeps its own copies: only the closed reader's go
/// when the grace runs out.
#[tokio::test]
async fn a_new_reader_s_copies_outlive_the_old_reader_s_grace() {
    let (mut f, opened) = confirmed_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Char('o')).await;
    press(&mut f, KeyCode::Char('q')).await;
    tick(&mut f, OPENED_GRACE_TICKS / 2);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Char('o')).await;
    let written = opened_paths(&opened);
    assert_eq!(written.len(), 2, "{written:?}");
    tick(&mut f, OPENED_GRACE_TICKS);
    all_gone(&written[..1]);
    all_there(&written[1..]);
}
