//! The attachment keys' own rules: `w` with no reader to save from, the open question's accept
//! and decline keys, and a filename whose control character must never reach the status line
//! raw. A child module of `tests/attachments.rs`, whose fixtures it uses, because the two
//! together would cross CONTRIBUTING's 800-line cap.

use super::*;

/// The hint bar, the last row of a 100x30 frame.
const BAR: usize = 29;

/// The `attachment.open` the fixture's first part asks for.
fn open_call() -> FakeCall {
    FakeCall::AttachmentOpen {
        account: "work".into(),
        message_id: "m1".into(),
        part_id: "0.1".into(),
    }
}

/// `a` then `Enter` on `plan.pdf`: the session's open question is on the screen, with nothing
/// sent yet.
async fn ask_to_open(f: &mut Fixture) {
    press(f, KeyCode::Char('a')).await;
    press(f, KeyCode::Enter).await;
    assert!(
        matches!(f.screen.modal, Some(Modal::Confirm(_))),
        "the open question is asked"
    );
    assert!(calls(f).is_empty(), "{:?}", calls(f));
}

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

/// FR-7: `y`, `Y` and `Enter` each answer the open question yes — the open is sent, the opener
/// is handed one URL, and the session flag is set, so the next `o` goes straight through.
#[tokio::test]
async fn y_and_enter_answer_the_open_question_yes() {
    for code in [KeyCode::Char('y'), KeyCode::Char('Y'), KeyCode::Enter] {
        let (mut f, opened) = attachment_fixture().await;
        ask_to_open(&mut f).await;
        press(&mut f, code).await;
        assert_eq!(calls(&f), [open_call()], "{code:?} did not open");
        assert_eq!(opened.lock().unwrap().len(), 1, "{code:?}: one URL");
        f.fake.calls.lock().unwrap().clear();
        press(&mut f, KeyCode::Char('a')).await;
        press(&mut f, KeyCode::Char('o')).await;
        assert!(
            f.screen.modal.is_none(),
            "{code:?} did not set the session flag"
        );
        assert_eq!(calls(&f), [open_call()], "{code:?}: the second open");
    }
}

/// FR-7: `n`, `N`, `q` and `Esc` each decline it: nothing is sent, nothing is opened, and the
/// list does not come back.
#[tokio::test]
async fn n_q_and_esc_decline_the_open_question() {
    for code in [
        KeyCode::Char('n'),
        KeyCode::Char('N'),
        KeyCode::Char('q'),
        KeyCode::Esc,
    ] {
        let (mut f, opened) = attachment_fixture().await;
        ask_to_open(&mut f).await;
        press(&mut f, code).await;
        assert!(calls(&f).is_empty(), "{code:?} sent {:?}", calls(&f));
        assert!(
            opened.lock().unwrap().is_empty(),
            "{code:?} opened something"
        );
        assert!(f.screen.modal.is_none(), "{code:?} left a modal");
    }
}

/// FR-18: a control character in a filename reaches the status line escaped. The **stored**
/// line is where that can be seen: the bar runs every row it draws through `fit`, which
/// escapes again, so even a raw name stored would be drawn escaped.
#[tokio::test]
async fn a_control_character_in_a_filename_is_escaped_on_the_status_line() {
    let parts = [("m1", "0.1", "plan\u{7}.pdf", 2048, "application/pdf")];
    let (mut f, _) = attachment_fixture_with(&parts, &[]).await;
    press(&mut f, KeyCode::Char('a')).await;
    answer_save(
        &f,
        vec![AttachmentResult {
            part_id: "0.1".into(),
            outcome: AttachmentOutcome::Failed(permanent(
                ErrorCode::Provider,
                "Gmail refused the attachment",
            )),
        }],
    );
    press(&mut f, KeyCode::Char('s')).await;
    let said = f.screen.status.notice().expect("a status line").to_string();
    assert_eq!(
        said,
        "Could not save plan\\u{7}.pdf: Gmail refused the attachment"
    );
    assert!(!said.contains('\u{7}'), "the raw character: {said:?}");
    let bar = rows(&f, 100, 30)[BAR].clone();
    assert!(bar.contains("plan\\u{7}.pdf"), "{bar:?}");
}
