//! The status line each attachment outcome gets: a Microsoft 365 reference attachment's own
//! words, a part the provider no longer has, a save where some parts failed, a refused or
//! missing destination, and each way an open can fail. A child module of
//! `tests/attachments.rs`, whose helpers it uses, because the two together would cross
//! CONTRIBUTING's 800-line cap.

use super::*;

/// AC-38: a Microsoft 365 reference attachment's own words reach the status line.
#[tokio::test]
async fn a_reference_attachment_is_said_on_the_status_line() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    let message = rail_core::provider::graph::REFERENCE_ATTACHMENT;
    answer_save(
        &f,
        vec![AttachmentResult {
            part_id: "0.1".into(),
            outcome: AttachmentOutcome::Failed(OpError::new(
                ErrorCode::Provider,
                ErrorClass::Permanent,
                message,
            )),
        }],
    );
    press(&mut f, KeyCode::Char('s')).await;
    assert_eq!(f.screen.status.notice(), Some(message));
    assert!(f.screen.status.notice_is_error());
}

/// AC-39: a part the provider no longer has names that provider.
#[tokio::test]
async fn a_missing_part_names_the_provider() {
    let missing = || {
        vec![AttachmentResult {
            part_id: "0.1".into(),
            outcome: AttachmentOutcome::Missing,
        }]
    };
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    answer_save(&f, missing());
    press(&mut f, KeyCode::Char('s')).await;
    assert_eq!(
        f.screen.status.notice(),
        Some("This attachment is no longer in Gmail.")
    );
    assert!(f.screen.status.notice_is_error());

    let (mut f, _) = attachment_fixture().await;
    f.screen.accounts[0].kind = AccountKind::M365;
    press(&mut f, KeyCode::Char('a')).await;
    answer_save(&f, missing());
    press(&mut f, KeyCode::Char('s')).await;
    assert_eq!(
        f.screen.status.notice(),
        Some("This attachment is no longer in Microsoft 365.")
    );
}

/// AC-40: a save where some parts failed says how many.
#[tokio::test]
async fn a_partial_save_says_how_many_failed() {
    let mut parts = PARTS.to_vec();
    parts.push(("m1", "0.3", "notes.txt", 128, "text/plain"));
    let (mut f, _) = attachment_fixture_with(&parts, &[]).await;
    press(&mut f, KeyCode::Char('a')).await;
    let dir = saved_dir(&f);
    let failed = permanent(ErrorCode::Provider, "Gmail refused the attachment");
    answer_save(
        &f,
        vec![
            saved_result("0.1", &dir.join("plan.pdf"), 2048),
            saved_result("0.2", &dir.join("logo.png"), 512),
            AttachmentResult {
                part_id: "0.3".into(),
                outcome: AttachmentOutcome::Failed(failed.clone()),
            },
        ],
    );
    press(&mut f, KeyCode::Char('S')).await;
    assert_eq!(
        f.screen.status.notice(),
        Some(
            format!(
                "Saved 2 of 3 attachments to {}; 1 failed: {}",
                saved_text(&f),
                failed.message
            )
            .as_str()
        )
    );
    assert!(f.screen.status.notice_is_error());
}

/// AC-41: a destination the daemon refused, and no destination at all, each get their line.
#[tokio::test]
async fn a_refused_destination_is_said_on_the_status_line() {
    let (mut f, _) = attachment_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    *f.fake.save_answer.lock().unwrap() = Some(Err(permanent(
        ErrorCode::InvalidInput,
        "dir: not an absolute path",
    )));
    press(&mut f, KeyCode::Char('s')).await;
    assert_eq!(
        f.screen.status.notice(),
        Some("Could not save the attachments: dir: not an absolute path")
    );
    assert!(f.screen.status.notice_is_error());

    let (mut f, _) = attachment_fixture().await;
    f.screen.ctx.attachment_dir = None;
    press(&mut f, KeyCode::Char('a')).await;
    press(&mut f, KeyCode::Char('s')).await;
    assert!(calls(&f).is_empty(), "{:?}", calls(&f));
    assert_eq!(
        f.screen.status.notice(),
        Some("Set [general] attachment_dir: Rail could not find your download directory.")
    );
}

/// AC-42: each way an open can fail gets its own line.
#[tokio::test]
async fn open_failures_each_get_their_own_line() {
    let (mut f, _) = confirmed_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    answer_open(
        &f,
        Err(permanent(
            ErrorCode::Unsupported,
            "Rail does not open this type",
        )),
    );
    press(&mut f, KeyCode::Char('o')).await;
    assert_eq!(
        f.screen.status.notice(),
        Some("Rail does not open this attachment's type.")
    );
    assert!(f.screen.status.notice_is_error());

    let (mut f, _) = confirmed_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    answer_open(
        &f,
        Err(permanent(ErrorCode::NotFound, "no such attachment")),
    );
    press(&mut f, KeyCode::Char('o')).await;
    assert_eq!(
        f.screen.status.notice(),
        Some("This attachment is no longer in Gmail.")
    );

    let (mut f, opened) = confirmed_fixture().await;
    press(&mut f, KeyCode::Char('a')).await;
    answer_open(
        &f,
        Ok(AttachmentOpenResponse {
            path: "cache/attachments/tok1/plan.pdf".into(),
        }),
    );
    press(&mut f, KeyCode::Char('o')).await;
    assert_eq!(
        f.screen.status.notice(),
        Some("Could not open the attachment: its path is not absolute")
    );
    assert!(opened.lock().unwrap().is_empty(), "nothing was opened");
}

/// §8's status lines, beyond AC-40: two saves in flight keep their own. The file a save named, and
/// the type an `Enter` would not open, travel with that save's own answer; they used to sit in one
/// screen-wide slot that the next save overwrote and the first answer emptied, so whichever answer
/// arrived first spoke for the wrong save.
#[tokio::test]
async fn two_saves_in_flight_keep_their_own_status_lines() {
    // `press` settles a key's effects before it returns, so the two saves are sent by hand and
    // answered afterwards: that is what has both of them in flight at once.
    let (mut f, _) = attachment_fixture().await;
    let failed = permanent(ErrorCode::Provider, "Gmail refused the attachment");
    press(&mut f, KeyCode::Char('a')).await;
    let first = f.screen.update(Msg::Key(key(KeyCode::Char('s'))));
    press(&mut f, KeyCode::Char('a')).await;
    highlight(&mut f, "logo.png").await;
    let second = f.screen.update(Msg::Key(key(KeyCode::Char('s'))));
    answer_save(&f, vec![failure("0.1", &failed)]);
    settle_capturing(&mut f.screen, first, &f.last_theme, &f.editor).await;
    assert_eq!(
        f.screen.status.notice(),
        Some(format!("Could not save plan.pdf: {}", failed.message).as_str())
    );
    answer_save(&f, vec![failure("0.2", &failed)]);
    settle_capturing(&mut f.screen, second, &f.last_theme, &f.editor).await;
    assert_eq!(
        f.screen.status.notice(),
        Some(format!("Could not save logo.png: {}", failed.message).as_str())
    );

    // The same for the type `Enter` would not open (FR-6): it belongs to the save that `Enter`
    // turned into, not to whichever save answers first.
    let mut parts = PARTS.to_vec();
    parts.push(("m1", "0.3", "page.html", 128, "text/html"));
    let (mut f, _) = attachment_fixture_with(&parts, &[]).await;
    let dir = saved_dir(&f);
    press(&mut f, KeyCode::Char('a')).await;
    let openable = f.screen.update(Msg::Key(key(KeyCode::Char('s'))));
    press(&mut f, KeyCode::Char('a')).await;
    highlight(&mut f, "page.html").await;
    let refused = f.screen.update(Msg::Key(key(KeyCode::Enter)));
    answer_save(&f, vec![saved_result("0.1", &dir.join("plan.pdf"), 2048)]);
    settle_capturing(&mut f.screen, openable, &f.last_theme, &f.editor).await;
    assert_eq!(
        f.screen.status.notice(),
        Some(format!("Saved plan.pdf to {}", saved_text(&f)).as_str())
    );
    answer_save(&f, vec![saved_result("0.3", &dir.join("page.html"), 128)]);
    settle_capturing(&mut f.screen, refused, &f.last_theme, &f.editor).await;
    assert_eq!(
        f.screen.status.notice(),
        Some(
            format!(
                "Rail does not open text/html files; saved page.html to {}",
                saved_text(&f)
            )
            .as_str()
        )
    );
}
