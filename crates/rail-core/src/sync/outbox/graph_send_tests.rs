//! The send path over the mock playing Graph's draft model: the draft is saved and its id kept
//! before the send, the send consumes it and leaves no delete, a restart reuses the saved
//! draft instead of making a second, a pushed draft is the one sent, a push still waiting when
//! the send runs makes no second draft, and Gmail's path saves no draft at all.

use super::sending_tests::{Harness, new_draft, queued_draft, sends};
use crate::ops::{Action, DraftId, OutboxId};
use crate::provider::mock::Call;
use crate::provider::{Carried, DraftIds, ProviderError};
use crate::store::drafts::{Attachment, DraftState};
use crate::store::outbox::NewAction;

fn draft_calls(h: &Harness) -> Vec<Call> {
    h.mock
        .calls()
        .into_iter()
        .filter(|c| matches!(c, Call::DraftCreate | Call::DraftUpdate | Call::DraftDelete))
        .collect()
}

fn outbox_rows(h: &Harness) -> i64 {
    h.with_store(|s| {
        s.conn()
            .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))
            .unwrap()
    })
}

/// The draft's provider ids, and whether it is stamped.
fn remote_of(h: &Harness, draft: DraftId) -> (Option<String>, Option<String>, bool) {
    let d = h.with_store(|s| s.draft(draft).unwrap().unwrap());
    (
        d.remote_draft_id,
        d.remote_message_id,
        d.send_attempted_at.is_some(),
    )
}

fn ids(draft: &str, message: &str) -> Option<DraftIds> {
    Some(DraftIds {
        draft_id: draft.into(),
        message_id: message.into(),
    })
}

async fn enqueue(h: &Harness, action: Action, token: &str, not_before: i64) -> OutboxId {
    h.enqueue(NewAction {
        account_id: h.account_id,
        action,
        threads: vec![],
        not_before,
        client_token: token.into(),
    })
    .await
}

#[tokio::test]
async fn a_graph_send_saves_the_draft_keeps_its_id_sends_it_and_leaves_no_delete() {
    let h = Harness::graph().await;
    let (draft, row) = queued_draft(&h, 1, vec![]).await;
    h.step().await;
    assert_eq!(draft_calls(&h), [Call::DraftCreate]);
    assert_eq!(sends(&h), 1);
    let sent = h.mock.with(|m| m.sent_mail.clone());
    assert_eq!(
        sent[0].remote,
        ids("r1", "dm1"),
        "the saved draft is the one sent"
    );
    assert!(h.row(row).await.is_none());
    assert!(
        h.with_store(|s| s.draft(draft).unwrap()).is_none(),
        "the local draft is gone"
    );
    assert_eq!(outbox_rows(&h), 0, "no delete: the send consumed the draft");
    assert!(h.mock.with(|m| m.drafts.is_empty()));
}

#[tokio::test]
async fn the_saved_drafts_id_is_kept_before_the_send_and_a_restart_reuses_it() {
    let h = Harness::graph().await;
    let (draft, row) = queued_draft(&h, 2, vec![]).await;
    assert_eq!(
        remote_of(&h, draft),
        (None, None, false),
        "before: nothing at Outlook"
    );
    h.mock
        .fail_next(Call::Send, ProviderError::transient("timed out"));
    h.step().await;
    assert_eq!(
        remote_of(&h, draft),
        (Some("r1".into()), Some("dm1".into()), true),
        "after: stamped, with the draft the send was for"
    );
    assert_eq!(
        h.with_store(|s| s.draft(draft).unwrap().unwrap().state),
        DraftState::Queued
    );
    // The daemon dies with the retry in flight; the next one checks first, then sends the
    // same draft.
    let pending = h.row(row).await.unwrap();
    h.advance_past(pending.not_before).await;
    h.claim(row).await;
    h.fresh_worker().recover_and_step().await;
    assert!(h.mock.calls().contains(&Call::FindByHeader));
    assert_eq!(
        draft_calls(&h),
        [Call::DraftCreate, Call::DraftUpdate],
        "the same draft, updated: no second one"
    );
    assert_eq!(h.mock.with(|m| m.sent.len()), 1, "sent once");
    assert_eq!(
        h.mock.with(|m| m.sent_mail[0].remote.clone()),
        ids("r1", "dm2")
    );
    assert!(h.row(row).await.is_none());
    assert_eq!(outbox_rows(&h), 0);
}

#[tokio::test]
async fn a_graph_send_found_sent_on_a_retry_completes_without_a_delete() {
    let h = Harness::graph().await;
    let (draft, row) = queued_draft(&h, 3, vec![]).await;
    h.with_store(|s| {
        s.draft_set_remote(draft, "r9", "r9").unwrap();
        s.conn()
            .execute(
                "UPDATE drafts SET send_attempted_at = 1 WHERE id = ?1",
                [draft],
            )
            .unwrap();
    });
    h.mock.with(|m| {
        m.known_header_ids.insert("<d3@example.com>".into());
    });
    h.step().await;
    assert_eq!(sends(&h), 0, "never sent a second time");
    assert!(h.row(row).await.is_none());
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_none());
    assert_eq!(
        outbox_rows(&h),
        0,
        "no delete: Outlook's copy is the sent message"
    );
}

#[tokio::test]
async fn a_pushed_draft_is_the_one_sent_and_no_second_draft_is_made() {
    let h = Harness::graph().await;
    let draft = new_draft(&h, 4, vec![]);
    enqueue(&h, Action::PushDraft { draft }, "push-4", 0).await;
    enqueue(&h, Action::Send { draft }, "send-4", 0).await;
    h.step().await; // the push: creates
    assert_eq!(
        remote_of(&h, draft),
        (Some("r1".into()), Some("dm1".into()), false)
    );
    h.step().await; // the send: updates that draft, then sends it
    assert_eq!(draft_calls(&h), [Call::DraftCreate, Call::DraftUpdate]);
    assert_eq!(h.mock.with(|m| m.sent_mail.len()), 1);
    assert_eq!(
        h.mock.with(|m| m.sent_mail[0].remote.clone()),
        ids("r1", "dm2")
    );
    assert!(
        h.mock.with(|m| m.drafts.is_empty()),
        "one draft, consumed by the send"
    );
    assert_eq!(outbox_rows(&h), 0, "no delete left behind");
}

#[tokio::test]
async fn a_send_that_runs_before_a_waiting_push_takes_the_push_with_it() {
    let h = Harness::graph().await;
    let draft = new_draft(&h, 5, vec![]);
    enqueue(&h, Action::Send { draft }, "send-5", 0).await;
    // A push row behind the send, still waiting (the claim rule keeps a draft's rows in
    // order, so this is the only way one waits while the send runs).
    h.with_store(|s| {
        s.conn()
            .execute(
                "INSERT INTO outbox (account_id, kind, payload, state, client_token, not_before,
                   thread_ids, reversal, draft_id)
                 VALUES (1, 'save_draft', ?1, 'pending', 'push-5', 4000000000, '[]', '[]', ?2)",
                rusqlite::params![
                    serde_json::to_string(&Action::PushDraft { draft }).unwrap(),
                    draft
                ],
            )
            .unwrap();
    });
    h.step().await;
    assert_eq!(
        draft_calls(&h),
        [Call::DraftCreate],
        "one draft, made by the send"
    );
    assert_eq!(h.mock.with(|m| m.sent_mail.len()), 1);
    assert_eq!(
        outbox_rows(&h),
        0,
        "the waiting push went with the sent draft"
    );
    h.step().await;
    assert_eq!(draft_calls(&h), [Call::DraftCreate], "nothing more");
}

#[tokio::test]
async fn a_gmail_send_saves_no_draft_first_and_records_no_ids() {
    let h = Harness::new().await;
    let (draft, _) = queued_draft(&h, 6, vec![]).await;
    h.mock
        .fail_next(Call::Send, ProviderError::permanent("refused"));
    h.step().await;
    assert!(
        draft_calls(&h).is_empty(),
        "Gmail sends the message, not a draft"
    );
    assert_eq!(remote_of(&h, draft), (None, None, true), "stamped, no ids");
}

#[tokio::test]
async fn a_graph_forward_carries_its_originals_without_reading_them() {
    let h = Harness::graph().await;
    let m1 = h.message_id("m1");
    let part = Attachment::Part {
        message: m1,
        part: "p1".into(),
        name: "orig.pdf".into(),
        mime: "application/pdf".into(),
        size: 3,
    };
    let (draft, _) = queued_draft(&h, 7, vec![part]).await;
    h.with_store(|s| {
        s.conn()
            .execute(
                "UPDATE drafts SET kind = 'forward', source_remote_id = 'm1' WHERE id = ?1",
                [draft],
            )
            .unwrap();
    });
    h.step().await;
    assert!(
        !h.mock.calls().contains(&Call::Attachment),
        "the original is not read"
    );
    let sent = h.mock.with(|m| m.sent_mail.clone());
    let attachment = &sent[0].outgoing.attachments[0];
    assert!(attachment.bytes.is_empty());
    assert_eq!(
        attachment.carried,
        Some(Carried {
            message: "m1".into(),
            part: "p1".into(),
            size: 3
        })
    );
    assert_eq!(sent[0].source.as_deref(), Some("m1"));
}
