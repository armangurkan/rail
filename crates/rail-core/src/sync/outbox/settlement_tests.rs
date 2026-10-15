//! A real SQLite trigger refuses settlement until storage is made writable again.

use std::time::Duration;

use tokio::task::JoinHandle;

use super::sending_tests::{Harness, new_draft, queued_draft};
use super::tests::WAIT;
use crate::ops::{Action, OutboxId};
use crate::provider::ProviderError;
use crate::provider::mock::Call;
use crate::store::drafts::DraftState;
use crate::store::outbox::{NewAction, OutboxState};

fn block(h: &Harness, row: OutboxId, event: &str) {
    h.with_store(|s| {
        s.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER refuse_settlement BEFORE {event} ON outbox
         WHEN OLD.id = {row} BEGIN SELECT RAISE(ABORT, 'test settlement failure'); END;"
            ))
            .unwrap()
    });
}

fn restore(h: &Harness) {
    h.with_store(|s| {
        s.conn()
            .execute_batch("DROP TRIGGER refuse_settlement")
            .unwrap()
    });
}

fn calls(h: &Harness, call: Call) -> usize {
    h.mock.calls().into_iter().filter(|c| *c == call).count()
}

/// Wait for the provider outcome, then keep the trigger installed while settlement fails.
async fn held(h: &Harness, row: OutboxId, call: Call) -> JoinHandle<()> {
    let mut step = tokio::spawn(h.worker_step());
    h.rig
        .until("the provider call", |_| calls(h, call) == 1)
        .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut step)
            .await
            .is_err(),
        "a failed settlement must retain the provider result until it can be recorded"
    );
    assert_eq!(h.row(row).await.unwrap().state, OutboxState::Inflight);
    step
}

async fn finish(h: &Harness, step: JoinHandle<()>) {
    restore(h);
    tokio::time::timeout(WAIT, step).await.unwrap().unwrap();
}

async fn action(h: &Harness, action: Action, token: &str) -> OutboxId {
    h.enqueue(NewAction {
        account_id: h.account_id,
        action,
        threads: vec![1],
        not_before: 0,
        client_token: token.into(),
    })
    .await
}

#[tokio::test]
async fn a_failed_completion_retries_the_write_and_unblocks_the_next_action() {
    let h = Harness::new().await;
    let first = action(&h, Action::Read { on: true }, "first").await;
    let next = action(&h, Action::Star { on: true }, "next").await;
    block(&h, first, "DELETE");
    let step = held(&h, first, Call::Modify).await;
    assert_eq!(h.row(next).await.unwrap().state, OutboxState::Pending);
    finish(&h, step).await;
    assert!(h.row(first).await.is_none());
    assert_eq!(calls(&h, Call::Modify), 1, "only the write is retried");
    h.step().await;
    assert!(h.row(next).await.is_none());
    assert_eq!(calls(&h, Call::Modify), 2);
}

#[tokio::test]
async fn a_failed_send_completion_does_not_send_the_message_again() {
    let h = Harness::new().await;
    let (draft, row) = queued_draft(&h, 62, vec![]).await;
    block(&h, row, "DELETE");
    let step = held(&h, row, Call::Send).await;
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_some());
    assert_eq!(h.sync_wakes(), 0);
    finish(&h, step).await;
    assert_eq!(calls(&h, Call::Send), 1);
    assert!(h.row(row).await.is_none());
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_none());
    assert_eq!(h.sync_wakes(), 1);
}

#[tokio::test]
async fn a_failed_push_completion_keeps_the_provider_ids_without_creating_again() {
    let h = Harness::new().await;
    let draft = new_draft(&h, 63, vec![]);
    let row = h
        .enqueue(NewAction {
            account_id: h.account_id,
            action: Action::PushDraft { draft },
            threads: vec![],
            not_before: 0,
            client_token: "push".into(),
        })
        .await;
    block(&h, row, "DELETE");
    let step = held(&h, row, Call::DraftCreate).await;
    assert!(
        h.with_store(|s| s.draft(draft).unwrap().unwrap().remote_draft_id)
            .is_none()
    );
    finish(&h, step).await;
    let remote = h
        .with_store(|s| s.draft(draft).unwrap().unwrap().remote_draft_id)
        .unwrap();
    assert!(h.mock.with(|m| m.drafts.contains_key(&remote)));
    assert_eq!(calls(&h, Call::DraftCreate), 1);
    assert_eq!(calls(&h, Call::DraftUpdate), 0);
    assert!(h.row(row).await.is_none());
    assert_eq!(h.sync_wakes(), 1);
}

#[tokio::test]
async fn a_failed_failure_write_counts_and_reports_the_provider_failure_once() {
    for permanent in [false, true] {
        let h = Harness::new().await;
        let (draft, row) = queued_draft(&h, 64, vec![]).await;
        let failure = if permanent {
            ProviderError::permanent("refused")
        } else {
            ProviderError::transient("unavailable")
        };
        h.mock.fail_next(Call::Send, failure);
        block(&h, row, "UPDATE OF attempts");
        let step = held(&h, row, Call::Send).await;
        assert_eq!(h.row(row).await.unwrap().attempts, 0);
        assert!(h.failures().is_empty());
        finish(&h, step).await;
        let failed = h.row(row).await.unwrap();
        assert_eq!(failed.attempts, 1);
        assert_eq!(calls(&h, Call::Send), 1);
        assert_eq!(h.failures().len(), usize::from(permanent));
        let draft = h.with_store(|s| s.draft(draft).unwrap().unwrap());
        assert_eq!(
            draft.state,
            if permanent {
                DraftState::Editing
            } else {
                DraftState::Queued
            }
        );
        assert_eq!(
            failed.state,
            if permanent {
                OutboxState::Failed
            } else {
                OutboxState::Pending
            }
        );
    }
}

#[tokio::test]
async fn a_failed_auth_release_retries_while_the_account_is_paused() {
    let h = Harness::new().await;
    let (draft, row) = queued_draft(&h, 65, vec![]).await;
    h.mock
        .fail_next(Call::Send, ProviderError::NeedsAuth { detail: None });
    // Claim also writes inflight_at; only the release should be refused.
    h.with_store(|s| {
        s.conn()
            .execute_batch(&format!(
                "CREATE TRIGGER refuse_settlement BEFORE UPDATE OF inflight_at ON outbox
         WHEN OLD.id = {row} AND NEW.state = 'pending'
         BEGIN SELECT RAISE(ABORT, 'test settlement failure'); END;"
            ))
            .unwrap()
    });
    let step = held(&h, row, Call::Send).await;
    assert!(h.rig.board.get(super::tests::ADDRESS).unwrap().needs_auth);
    finish(&h, step).await;
    let released = h.row(row).await.unwrap();
    assert_eq!(released.state, OutboxState::Pending);
    assert_eq!(released.attempts, 0);
    assert_eq!(calls(&h, Call::Send), 1);
    assert_eq!(
        h.with_store(|s| s.draft(draft).unwrap().unwrap().state),
        DraftState::Queued
    );
}

#[tokio::test]
async fn a_dead_writer_ends_settlement_retries_without_repeating_the_send() {
    let h = Harness::new().await;
    let (_, row) = queued_draft(&h, 66, vec![]).await;
    block(&h, row, "DELETE");
    let step = held(&h, row, Call::Send).await;
    let poison = crate::sync::batch::Batch {
        panic_writer: true,
        ..crate::sync::batch::Batch::new(h.account_id)
    };
    assert!(h.rig.writer.write(poison).await.is_err());
    tokio::time::timeout(WAIT, step).await.unwrap().unwrap();
    assert_eq!(calls(&h, Call::Send), 1);
    assert_eq!(h.row(row).await.unwrap().state, OutboxState::Inflight);
}
