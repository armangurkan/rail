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
