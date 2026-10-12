//! The composer's edges: a reply whose message leaves the store, forwarded parts removed,
//! the attachment size estimate, compose files swept at start, and a late thread read.

use std::sync::{Arc, Mutex};

use rail_core::ops::DraftSaveResponse;
use rail_core::ops::drafts::{AttachmentSummary, DraftAttachment};

use super::compose::{compose_fixture, composing, editor_appends, saves, type_text};
use super::*;
use crate::tui::screens::compose::RowKind;

/// Answers each save as the daemon would about the message a reply names: refused when it
/// is no longer in the store. `requests` records every request.
fn daemon_like_saves(f: &Fixture, requests: Arc<Mutex<Vec<rail_core::ops::DraftSaveRequest>>>) {
    let db = f.paths.db_file();
    *f.fake.on_save.lock().unwrap() = Some(Box::new(move |call| {
        requests.lock().unwrap().push(call.request.clone());
        if let Some(reply) = &call.request.reply_to {
            let store = Store::open(&db).unwrap();
            if store.reply_source(reply.message).unwrap().is_none() {
                return Err(OpError::not_found(
                    "reply_to: the message being answered is no longer in the store",
                ));
            }
        }
        Ok(DraftSaveResponse {
            draft: 1,
            message_id: "<x@example.com>".into(),
            attachments: vec![],
        })
    }));
}

/// Final fix 1: only the create names the message answered; a later save keeps the stored
/// threading, so it still saves once that message is gone.
#[tokio::test]
async fn a_reply_still_saves_after_its_message_leaves_the_store() {
    let mut f = compose_fixture().await;
    editor_appends(&f, "more\n");
    let requests = Arc::new(Mutex::new(Vec::new()));
    daemon_like_saves(&f, requests.clone());
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await;
    let first = requests.lock().unwrap()[0].clone();
    let source = first
        .reply_to
        .expect("the create names the message")
        .message;
    assert_eq!(first.draft, None);
    writer(&f)
        .conn()
        .execute("DELETE FROM messages WHERE id = ?1", [source])
        .unwrap();
    press(&mut f, KeyCode::Char('e')).await;
    let sent = requests.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        3,
        "create, after the editor, after the second edit"
    );
    assert!(
        sent[1..].iter().all(|r| r.reply_to.is_none()),
        "updates name no message"
    );
    assert!(
        composing(&f).errors.is_empty(),
        "{:?}",
        composing(&f).errors
    );
    assert!(!f.screen.composing.unsaved);
    assert_eq!(saves(&f).len(), 3);
}

/// A summary of the forwarded part `p1` of `message`.
fn part_summary(message: i64) -> AttachmentSummary {
    AttachmentSummary {
        name: "plan.pdf".into(),
        mime: "application/pdf".into(),
        size: 1234,
        path: None,
        message: Some(message),
        part: Some("p1".into()),
    }
}

/// Answers each save as the daemon does with parts: a forward's create takes `p1` of the
/// message it names; an update keeps the parts it lists.
fn daemon_like_parts(f: &Fixture, requests: Arc<Mutex<Vec<rail_core::ops::DraftSaveRequest>>>) {
    *f.fake.on_save.lock().unwrap() = Some(Box::new(move |call| {
        let request = &call.request;
        requests.lock().unwrap().push(request.clone());
        let attachments = match (request.draft, &request.reply_to) {
            (None, Some(reply)) => vec![part_summary(reply.message)],
            _ => request
                .attachments
                .iter()
                .filter_map(|a| match a {
                    DraftAttachment::Part { message, .. } => Some(part_summary(*message)),
                    DraftAttachment::File { .. } => None,
                })
                .collect(),
        };
        Ok(DraftSaveResponse {
            draft: 1,
            message_id: "<x@example.com>".into(),
            attachments,
        })
    }));
}

fn lists_a_part(request: &rail_core::ops::DraftSaveRequest) -> bool {
    request
        .attachments
        .iter()
        .any(|a| matches!(a, DraftAttachment::Part { part, .. } if part == "p1"))
}

/// Final fix 2: an update lists the forwarded parts kept, so `x` on one removes it.
#[tokio::test]
async fn x_removes_a_forwarded_part_and_later_saves_leave_it_out() {
    let mut f = compose_fixture().await;
    let store = writer(&f);
    let message: i64 = store
        .conn()
        .query_row("SELECT id FROM messages WHERE remote_id = 'wm2'", [], |r| {
            r.get(0)
        })
        .unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO attachments (message_id, remote_part_id, filename, mime_type, size)
             VALUES (?1, 'p1', 'plan.pdf', 'application/pdf', 1234)",
            [message],
        )
        .unwrap();
    editor_appends(&f, "");
    let requests = Arc::new(Mutex::new(Vec::new()));
    daemon_like_parts(&f, requests.clone());
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('f')).await;
    {
        let sent = requests.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(!lists_a_part(&sent[0]), "a create names no part");
        assert!(lists_a_part(&sent[1]), "an update lists the part it keeps");
    }
    assert_eq!(
        composing(&f).attachments.len(),
        1,
        "the part is on the review"
    );
    while f.screen.compose.as_ref().unwrap().cursor_row_kind() != RowKind::Attachment(0) {
        press(&mut f, KeyCode::Char('j')).await;
    }
    press(&mut f, KeyCode::Char('x')).await;
    let last = requests.lock().unwrap().last().cloned().unwrap();
    assert!(
        !lists_a_part(&last),
        "the removal's save leaves the part out"
    );
    assert!(composing(&f).attachments.is_empty());
}

/// Final fix 3: `a` refuses what the worker would, counting attachments as encoded.
#[tokio::test]
async fn a_refuses_a_file_over_the_limit_once_encoded_without_a_save() {
    let mut f = compose_fixture().await;
    editor_appends(&f, "");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("big.bin");
    std::fs::File::create(&file)
        .unwrap()
        .set_len(20 * 1024 * 1024)
        .unwrap();
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await;
    let before = saves(&f).len();
    press(&mut f, KeyCode::Char('a')).await;
    type_text(&mut f, &file.to_string_lossy()).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(saves(&f).len(), before, "refused before any save");
    assert!(composing(&f).attachments.is_empty());
    let error = &composing(&f).errors[0];
    assert!(
        error.contains("25 MiB") && error.contains("26.6 MiB"),
        "{error}"
    );
}
