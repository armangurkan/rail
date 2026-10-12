//! The push and Gmail-delete paths over `MockProvider` and a real writer: a first push creates
//! and a second updates the same Gmail draft, an update of a draft deleted at Gmail creates it
//! again, a push into a thread gone at Gmail is saved on its own, a delete row (a 404 is done),
//! a sent pushed draft's Gmail copy deleted (and a failed delete retried), failures, a stop
//! during a create, and the saving depth.

use std::time::Duration;

use mail_parser::MessageParser;

use super::sending_tests::{Harness, new_draft};
use crate::ops::{Action, DraftId, OutboxId};
use crate::provider::mock::Call;
use crate::provider::{Provider, ProviderError};
use crate::store::drafts::DraftState;
use crate::store::outbox::{NewAction, OutboxOutcome, OutboxWrite};

async fn push(h: &Harness, draft: DraftId, token: &str) -> OutboxId {
    h.enqueue(NewAction {
        account_id: h.account_id,
        action: Action::PushDraft { draft },
        threads: vec![],
        not_before: 0,
        client_token: token.into(),
    })
    .await
}

/// The draft's Gmail draft id and Gmail message id.
fn remote_ids(h: &Harness, draft: DraftId) -> (Option<String>, Option<String>) {
    let d = h.with_store(|s| s.draft(draft).unwrap().unwrap());
    (d.remote_draft_id, d.remote_message_id)
}

fn ids(draft: &str, message: &str) -> (Option<String>, Option<String>) {
    (Some(draft.into()), Some(message.into()))
}

/// The subject and text body of the mock's Gmail draft `id`.
fn gmail_copy(h: &Harness, id: &str) -> (String, String) {
    let raw = h.mock.with(|m| m.drafts[id].raw.clone());
    let parsed = MessageParser::default().parse(&raw).unwrap();
    (
        parsed.subject().unwrap_or_default().to_string(),
        parsed
            .body_text(0)
            .unwrap_or_default()
            .trim_end()
            .to_string(),
    )
}

fn set_body(h: &Harness, draft: DraftId, body: &str) {
    h.with_store(|s| {
        s.conn()
            .execute(
                "UPDATE drafts SET body = ?2 WHERE id = ?1",
                rusqlite::params![draft, body],
            )
            .unwrap();
    });
}

fn draft_calls(h: &Harness) -> Vec<Call> {
    h.mock
        .calls()
        .into_iter()
        .filter(|c| {
            matches!(
                c,
                Call::DraftCreate | Call::DraftUpdate | Call::DraftDelete | Call::DraftList
            )
        })
        .collect()
}

/// Mark `draft` as held by Gmail as `remote`, and discard it: the Gmail delete row it leaves.
async fn discard_pushed(h: &Harness, draft: DraftId, remote: &str) {
    h.with_store(|s| {
        s.conn()
            .execute(
                "UPDATE drafts SET remote_draft_id = ?2, remote_message_id = 'dm-x' WHERE id = ?1",
                rusqlite::params![draft, remote],
            )
            .unwrap();
    });
    h.rig
        .writer
        .outbox(OutboxWrite::DraftDelete(draft))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_first_push_creates_and_a_second_updates_the_same_gmail_draft() {
    let h = Harness::new().await;
    let draft = new_draft(&h, 1, vec![]);
    let first = push(&h, draft, "push-1").await;
    h.step().await;
    assert_eq!(draft_calls(&h), [Call::DraftCreate]);
    assert_eq!(remote_ids(&h, draft), ids("r1", "dm1"));
    assert_eq!(gmail_copy(&h, "r1"), ("hello 1".into(), "hi".into()));
    assert!(h.row(first).await.is_none(), "the row completed");
    assert_eq!(h.sync_wakes(), 1, "the sync engine is nudged");

    set_body(&h, draft, "edited\n");
    let second = push(&h, draft, "push-2").await;
    h.step().await;
    assert_eq!(draft_calls(&h), [Call::DraftCreate, Call::DraftUpdate]);
    assert_eq!(remote_ids(&h, draft), ids("r1", "dm2"));
    assert_eq!(gmail_copy(&h, "r1"), ("hello 1".into(), "edited".into()));
    assert_eq!(h.mock.with(|m| m.drafts.len()), 1, "the same Gmail draft");
    assert!(h.row(second).await.is_none());
    assert_eq!(h.sync_wakes(), 2, "nudged after each push");
}

#[tokio::test]
async fn an_update_of_a_draft_deleted_at_gmail_creates_it_again() {
    let h = Harness::new().await;
    let draft = new_draft(&h, 2, vec![]);
    push(&h, draft, "push-1").await;
    h.step().await;
    assert_eq!(remote_ids(&h, draft), ids("r1", "dm1"));
    h.mock.with(|m| {
        m.missing_drafts.insert("r1".into());
    });
    let row = push(&h, draft, "push-2").await;
    h.step().await;
    assert_eq!(
        draft_calls(&h),
        [Call::DraftCreate, Call::DraftUpdate, Call::DraftCreate],
        "the update is answered 404, then a create"
    );
    assert_eq!(remote_ids(&h, draft), ids("r2", "dm2"));
    assert!(h.row(row).await.is_none(), "the row completed");
    assert!(h.failures().is_empty());
}

#[tokio::test]
async fn a_push_into_a_thread_gone_at_gmail_is_saved_on_its_own() {
    let h = Harness::new().await;
    let draft = new_draft(&h, 3, vec![]);
    h.with_store(|s| {
        s.conn().execute("UPDATE drafts SET kind = 'reply', remote_thread_id = 't-deleted', in_reply_to = '<m1@example.com>' WHERE id = ?1", [draft]).unwrap();
    });
    h.mock
        .with(|m| m.missing_threads.insert("t-deleted".into()));
    let row = push(&h, draft, "push-1").await;
    h.step().await;
    assert_eq!(draft_calls(&h), [Call::DraftCreate, Call::DraftCreate]);
    let held = h.mock.with(|m| m.drafts.clone());
    assert_eq!(held.len(), 1, "saved once");
    let (id, copy) = held.iter().next().unwrap();
    assert_eq!(copy.thread_id, None, "saved without the thread");
    assert_eq!(remote_ids(&h, draft).0.as_deref(), Some(id.as_str()));
    assert!(h.row(row).await.is_none(), "the row completed");
}

#[tokio::test]
async fn a_delete_row_deletes_at_gmail_and_a_404_is_done() {
    let h = Harness::new().await;
    let created = h
        .mock
        .draft_create(b"Subject: x\r\n\r\nx", None)
        .await
        .unwrap();
    let held = new_draft(&h, 4, vec![]);
    discard_pushed(&h, held, &created.draft_id).await;
    h.step().await;
    assert_eq!(draft_calls(&h), [Call::DraftCreate, Call::DraftDelete]);
    assert!(h.mock.with(|m| m.drafts.is_empty()), "deleted at Gmail");
    // Gone at Gmail already: a 404 is done.
    let gone = new_draft(&h, 5, vec![]);
    discard_pushed(&h, gone, "r-gone").await;
    h.step().await;
    assert_eq!(
        draft_calls(&h),
        [Call::DraftCreate, Call::DraftDelete, Call::DraftDelete]
    );
    assert!(h.failures().is_empty());
    let depth = h.with_store(|s| s.outbox_depth(h.account_id).unwrap());
    assert_eq!((depth.pending, depth.failed), (0, 0), "both rows completed");
}

#[tokio::test]
async fn a_sent_pushed_draft_deletes_its_gmail_copy_and_a_failed_delete_can_be_retried() {
    let h = Harness::new().await;
    let draft = new_draft(&h, 6, vec![]);
    push(&h, draft, "push-1").await;
    h.step().await;
    assert_eq!(remote_ids(&h, draft), ids("r1", "dm1"));
    let send = h
        .enqueue(NewAction {
            account_id: h.account_id,
            action: Action::Send { draft },
            threads: vec![],
            not_before: 0,
            client_token: "send-6".into(),
        })
        .await;
    h.step().await;
    assert_eq!(h.mock.with(|m| m.sent.len()), 1, "sent once");
    h.mock.fail_next(
        Call::DraftDelete,
        ProviderError::permanent("Gmail refused the request (HTTP 400)"),
    );
    h.step().await;
    let failed = h.failures();
    assert_eq!(failed.len(), 1);
    assert_eq!(
        failed[0].action,
        Action::DeleteDraft {
            draft,
            remote_draft_id: "r1".into(),
        }
    );
    assert_eq!(failed[0].client_token, format!("after-send-{send}"));
    assert!(
        h.mock.with(|m| m.drafts.contains_key("r1")),
        "not deleted yet"
    );
    let retried = h
        .rig
        .writer
        .outbox(OutboxWrite::Retry {
            id: failed[0].row,
            client_token: failed[0].client_token.clone(),
        })
        .await
        .unwrap();
    assert_eq!(retried, OutboxOutcome::Retried(true));
    h.step().await;
    assert!(
        h.mock.with(|m| m.drafts.is_empty()),
        "the Gmail copy is gone"
    );
    assert_eq!(h.mock.with(|m| m.sent.len()), 1, "still sent once");
    assert!(h.row(failed[0].row).await.is_none());
}

#[tokio::test]
async fn a_push_fails_transiently_and_permanently_like_any_action() {
    let h = Harness::new().await;
    let draft = new_draft(&h, 7, vec![]);
    let before = h.with_store(|s| s.draft(draft).unwrap().unwrap());
    let row = push(&h, draft, "push-7").await;
    h.mock.fail_next(
        Call::DraftCreate,
        ProviderError::transient("Gmail is unavailable (HTTP 503)"),
    );
    h.step().await;
    let pending = h.row(row).await.unwrap();
    assert_eq!(pending.attempts, 1, "pending again with an attempt counted");
    assert!(h.failures().is_empty());
    assert_eq!(remote_ids(&h, draft), (None, None));
    h.advance_past(pending.not_before).await;
    h.mock.fail_next(
        Call::DraftCreate,
        ProviderError::permanent("Gmail refused the request (HTTP 400)"),
    );
    h.step().await;
    let failed = h.failures();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].action, Action::PushDraft { draft });
    assert_eq!(failed[0].draft, Some(draft));
    assert_eq!(failed[0].client_token, "push-7");
    let after = h.with_store(|s| s.draft(draft).unwrap().unwrap());
    assert_eq!(after.state, DraftState::Editing);
    assert_eq!(
        (after.body, after.remote_draft_id, after.updated_at),
        (before.body, None, before.updated_at),
        "the local draft is unchanged"
    );
}

#[tokio::test]
async fn a_stop_lets_a_push_in_flight_finish_and_record_its_ids() {
    let mut h = Harness::new().await;
    h.mock.with(|m| m.action_delay = Duration::from_millis(200));
    let draft = new_draft(&h, 8, vec![]);
    let row = push(&h, draft, "push-8").await;
    h.rig.start();
    h.rig
        .until("pushing", |r| r.mock.calls().contains(&Call::DraftCreate))
        .await;
    h.rig.stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), h.rig.task.take().unwrap())
        .await
        .expect("stopped in time")
        .unwrap();
    assert_eq!(
        remote_ids(&h, draft),
        ids("r1", "dm1"),
        "the ids are stored"
    );
    assert!(h.row(row).await.is_none(), "recorded as pushed");
}

#[tokio::test]
async fn the_status_shows_saving_while_a_push_runs() {
    let h = Harness::new().await;
    h.mock.with(|m| m.action_delay = Duration::from_millis(200));
    let draft = new_draft(&h, 9, vec![]);
    push(&h, draft, "push-9").await;
    let step = tokio::spawn(h.worker_step());
    tokio::time::sleep(Duration::from_millis(50)).await;
    let during = h.depth().await;
    assert_eq!((during.saving, during.sending), (1, 0));
    step.await.unwrap();
    assert_eq!(h.depth().await.saving, 0);
}
