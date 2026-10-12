//! The composer's edges: a reply whose message leaves the store, forwarded parts removed,
//! the attachment size estimate, compose files swept at start, and a late thread read.

use std::sync::{Arc, Mutex};

use rail_core::ops::DraftSaveResponse;

use super::compose::{compose_fixture, composing, editor_appends, saves};
use super::*;

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
