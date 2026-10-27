//! The attachment keys' own rules, starting with `w` when there is no reader to save from. A
//! child module of `tests/attachments.rs`, whose fixtures it uses, because the two together
//! would cross CONTRIBUTING's 800-line cap.

use super::*;

/// The status line as it stands, so a key that must say nothing can be held to it.
fn said(f: &Fixture) -> Option<String> {
    f.screen.status.notice().map(ToString::to_string)
}

/// FR-10: `w` with no reader open saves nothing and says nothing. Twice over: from the list,
/// where the key is not bound at all, and with the reader gone from under a reader focus,
/// which is what `reader_action`'s own guard is for.
#[tokio::test]
async fn w_without_a_reader_saves_nothing_and_says_nothing() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('q')).await;
    assert!(f.screen.reader.is_none(), "`q` closed the reader");
    let before = said(&f);
    press(&mut f, KeyCode::Char('w')).await;
    assert!(calls(&f).is_empty(), "the list's `w` sent {:?}", calls(&f));
    assert_eq!(said(&f), before, "the list's `w` said something");
    assert!(f.screen.modal.is_none(), "the list's `w` drew a modal");

    let (mut f, _) = attachment_fixture().await;
    f.screen.reader = None;
    f.screen.focus = Focus::Reader;
    let before = said(&f);
    press(&mut f, KeyCode::Char('w')).await;
    assert!(calls(&f).is_empty(), "`w` sent {:?}", calls(&f));
    assert_eq!(said(&f), before, "`w` with no reader said something");
    assert_eq!(
        f.screen.focus,
        Focus::List,
        "the focus fell back to the list"
    );
}
