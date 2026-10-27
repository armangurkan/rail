//! An open the daemon refused because the bytes are not what the declared type says (#107):
//! the opener is never called, and the part is saved instead, as a type Rail does not open is.
//! A child module of `tests/attachments.rs`, whose helpers it uses.

use rail_core::ops::CONTENT_MISMATCH;

use super::*;

/// The daemon's refusal of markup declared `image/png`.
fn mismatch() -> OpError {
    permanent(ErrorCode::Unsupported, CONTENT_MISMATCH)
}

/// `logo.png`, whose bytes the daemon found to be `<svg onload=…>`, is not opened: the same
/// part is saved instead and the line says why.
#[tokio::test]
async fn a_png_whose_bytes_are_svg_is_saved_and_never_opened() {
    let (mut f, opened) = confirmed_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    highlight(&mut f, "logo.png").await;
    answer_open(&f, Err(mismatch()));
    let dir = saved_dir(&f);
    answer_save(&f, vec![saved_result("0.2", &dir.join("logo.png"), 512)]);
    press(&mut f, KeyCode::Char('o')).await;
    assert_eq!(
        calls(&f),
        [
            FakeCall::AttachmentOpen {
                account: "work".into(),
                message_id: "m1".into(),
                part_id: "0.2".into(),
            },
            save_call("work", "m1", &["0.2"], &saved_text(&f)),
        ]
    );
    assert!(opened.lock().unwrap().is_empty(), "the opener was called");
    assert_eq!(
        f.screen.status.notice(),
        Some(
            format!(
                "Rail does not open a file whose contents do not match its type; saved logo.png to {}",
                saved_text(&f)
            )
            .as_str()
        )
    );
    assert!(!f.screen.status.notice_is_error());
    assert!(
        rows(&f, 90, 24).iter().all(|row| !row.contains("svg")),
        "nothing of the bytes is drawn"
    );
}

/// With nowhere to save, the refusal still opens nothing and says where to set a directory.
#[tokio::test]
async fn a_mismatch_with_no_destination_opens_nothing() {
    let (mut f, opened) = confirmed_fixture().await;
    f.screen.ctx.attachment_dir = None;
    press(&mut f, KeyCode::Char('a')).await;
    answer_open(&f, Err(mismatch()));
    press(&mut f, KeyCode::Char('o')).await;
    assert!(opened.lock().unwrap().is_empty());
    assert_eq!(calls(&f).len(), 1, "only the open: {:?}", calls(&f));
    assert_eq!(
        f.screen.status.notice(),
        Some("Set [general] attachment_dir: Rail could not find your download directory.")
    );
}
