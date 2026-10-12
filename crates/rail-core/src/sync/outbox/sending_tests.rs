//! The send path over `MockProvider` and a real writer: the message built from the draft, the
//! draft gone and sync nudged once sent, the never-twice rule (a retry and a row found in
//! flight at start look the message up first), permanent refusals back to editing with the
//! draft and token in the notice, attachments read and fetched, the sending depth, shutdown.

use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, Waker};
use std::time::Duration;

use tokio::sync::Notify;

use super::Worker;
use super::tests::{ADDRESS, Rig, rig};
use crate::ops::{Action, ActionFailed, DraftId, OutboxDepth, OutboxId};
use crate::provider::ProviderError;
use crate::provider::mock::{Call, MockProvider};
use crate::store::Store;
use crate::store::drafts::{Attachment, DraftKind, DraftState, DraftWrite, Recipient};
use crate::store::outbox::{NewAction, OutboxOutcome, OutboxRow, OutboxWrite};

struct Harness {
    rig: Rig,
    account_id: i64,
    mock: MockProvider,
    from_name: Option<String>,
    sync_wake: Arc<Notify>,
    wakes: AtomicUsize,
    worker: Arc<Worker>,
}

impl Harness {
    async fn new() -> Self {
        Self::build(None).await
    }

    async fn with_from_name(name: &str) -> Self {
        Self::build(Some(name.to_string())).await
    }

    async fn build(from_name: Option<String>) -> Self {
        let rig = rig().await;
        let sync_wake = Arc::new(Notify::new());
        let worker = Arc::new(rig.worker_with(from_name.clone(), sync_wake.clone()));
        Self {
            mock: rig.mock.clone(),
            rig,
            account_id: 1,
            from_name,
            sync_wake,
            wakes: AtomicUsize::new(0),
            worker,
        }
    }

    fn with_store<R>(&self, f: impl FnOnce(&mut Store) -> R) -> R {
        f(&mut Store::open(&self.rig.db).unwrap())
    }

    async fn enqueue(&self, new: NewAction) -> OutboxId {
        let OutboxOutcome::Enqueued { rows, .. } = self
            .rig
            .writer
            .outbox(OutboxWrite::Enqueue(new))
            .await
            .unwrap()
        else {
            panic!("enqueued");
        };
        rows[0]
    }

    async fn step(&self) {
        self.worker.step().await;
    }

    /// One step of the harness's worker, as a task of its own.
    fn worker_step(&self) -> impl Future<Output = ()> + Send + 'static {
        let worker = self.worker.clone();
        async move {
            worker.step().await;
        }
    }

    async fn row(&self, id: OutboxId) -> Option<OutboxRow> {
        self.rig.store().outbox_row(id).unwrap()
    }

    /// Make every row due at or before `not_before` due now (the writer claims by wall time).
    async fn advance_past(&self, not_before: i64) {
        self.with_store(|s| {
            s.conn()
                .execute(
                    "UPDATE outbox SET not_before = 0 WHERE not_before <= ?1",
                    [not_before],
                )
                .unwrap();
        });
    }

    /// Leave `id` inflight, as a daemon that died mid-send would.
    async fn claim(&self, id: OutboxId) {
        let claimed = self
            .rig
            .writer
            .outbox(OutboxWrite::Claim {
                account_id: self.account_id,
            })
            .await
            .unwrap();
        assert!(matches!(claimed, OutboxOutcome::Claimed(Some(row)) if row.id == id));
    }

    /// A second worker over the same store, as after a daemon restart.
    fn fresh_worker(&self) -> Worker {
        self.rig
            .worker_with(self.from_name.clone(), self.sync_wake.clone())
    }

    fn failures(&self) -> Vec<ActionFailed> {
        self.rig.failures.lock().unwrap().clone()
    }

    async fn depth(&self) -> OutboxDepth {
        self.rig.board.get(ADDRESS).unwrap().outbox
    }

    /// How many times the sync engine was nudged so far (a stored permit is taken and counted).
    fn sync_wakes(&self) -> usize {
        let mut notified = pin!(self.sync_wake.notified());
        if notified
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            == Poll::Ready(())
        {
            self.wakes.fetch_add(1, Ordering::SeqCst);
        }
        self.wakes.load(Ordering::SeqCst)
    }

    /// The store id of the message whose provider id is `remote`.
    fn message_id(&self, remote: &str) -> i64 {
        self.rig
            .store()
            .conn()
            .query_row(
                "SELECT id FROM messages WHERE remote_id = ?1",
                [remote],
                |r| r.get(0),
            )
            .unwrap()
    }
}

impl Worker {
    /// What `run` does first, then one step.
    async fn recover_and_step(&self) {
        self.recover().await;
        self.step().await;
    }
}

/// An `editing` draft of the account, its Message-ID `<d{n}@example.com>`.
fn new_draft(h: &Harness, n: i64, attachments: Vec<Attachment>) -> DraftId {
    h.with_store(|s| {
        s.draft_upsert(
            &DraftWrite {
                id: None,
                account_id: h.account_id,
                kind: DraftKind::New,
                thread_id: None,
                remote_thread_id: None,
                in_reply_to: None,
                references: None,
                to: vec![Recipient {
                    name: Some("Sam".into()),
                    address: "sam@example.com".into(),
                }],
                cc: vec![],
                bcc: vec![],
                subject: format!("hello {n}"),
                body: "hi\n".into(),
                attachments,
                message_id: format!("<d{n}@example.com>"),
            },
            1,
        )
        .unwrap()
        .unwrap()
    })
}

async fn queued_draft(h: &Harness, n: i64, attachments: Vec<Attachment>) -> (DraftId, OutboxId) {
    let draft = new_draft(h, n, attachments);
    let row = h
        .enqueue(NewAction {
            account_id: h.account_id,
            action: Action::Send { draft },
            threads: vec![],
            not_before: 0,
            client_token: format!("tok-{n}"),
        })
        .await;
    (draft, row)
}

fn sends(h: &Harness) -> usize {
    h.mock.calls().iter().filter(|c| **c == Call::Send).count()
}

#[tokio::test]
async fn a_send_row_builds_the_message_sends_it_deletes_the_draft_and_wakes_sync() {
    let h = Harness::new().await;
    let (draft, _row) = queued_draft(&h, 1, vec![]).await;
    h.step().await;
    let sent = h.mock.with(|m| m.sent.clone());
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].thread_id, None);
    let raw = String::from_utf8_lossy(&sent[0].raw);
    assert!(raw.contains("Message-ID: <d1@example.com>"), "{raw}");
    assert!(
        raw.contains("To: \"Sam\" <sam@example.com>") || raw.contains("To: Sam <sam@example.com>"),
        "{raw}"
    );
    assert!(
        raw.contains("From: <me@example.com>") || raw.contains("From: me@example.com"),
        "{raw}"
    );
    assert!(
        h.with_store(|s| s.draft(draft).unwrap()).is_none(),
        "the draft is gone once sent"
    );
    assert_eq!(h.sync_wakes(), 1, "the sync engine is nudged");
    assert_eq!(
        h.depth().await,
        OutboxDepth {
            pending: 0,
            failed: 0,
            sending: 0,
            saving: 0,
        }
    );
}

#[tokio::test]
async fn a_from_name_and_a_reply_carry_their_headers_and_thread() {
    let h = Harness::with_from_name("Dana Reyes").await;
    let (draft, _) = queued_draft(&h, 2, vec![]).await;
    h.with_store(|s| {
        s.conn().execute(
            "UPDATE drafts SET kind = 'reply', remote_thread_id = 't1', in_reply_to = '<m1@example.com>', references_ids = '<m0@example.com> <m1@example.com>' WHERE id = ?1",
            [draft],
        ).unwrap();
    });
    h.step().await;
    let sent = h.mock.with(|m| m.sent.clone());
    assert_eq!(sent[0].thread_id.as_deref(), Some("t1"));
    let raw = String::from_utf8_lossy(&sent[0].raw);
    assert!(
        raw.contains("From: \"Dana Reyes\" <me@example.com>")
            || raw.contains("From: Dana Reyes <me@example.com>"),
        "{raw}"
    );
    assert!(raw.contains("In-Reply-To: <m1@example.com>"), "{raw}");
    assert!(
        raw.contains("References: <m0@example.com> <m1@example.com>")
            || raw.contains("References: <m0@example.com>\r\n <m1@example.com>"),
        "{raw}"
    );
}

#[tokio::test]
async fn a_retry_looks_the_message_up_first_and_does_not_send_twice() {
    let h = Harness::new().await;
    let (_, row) = queued_draft(&h, 3, vec![]).await;
    // The first attempt fails after Gmail got it (a timeout reading the answer).
    h.mock
        .fail_next(Call::Send, ProviderError::transient("timed out"));
    h.step().await;
    assert_eq!(
        h.with_store(|s| s.failed_rows(h.account_id).unwrap()).len(),
        0,
        "transient: pending again"
    );
    let pending = h.row(row).await.unwrap();
    assert_eq!(pending.attempts, 1);
    // Gmail does have it.
    h.mock.with(|m| {
        m.known_header_ids.insert("<d3@example.com>".into());
    });
    h.advance_past(pending.not_before).await;
    h.step().await;
    assert_eq!(sends(&h), 1, "the retry never resent");
    assert!(h.mock.calls().contains(&Call::FindByHeader));
    assert!(h.row(row).await.is_none(), "completed as sent");
}

#[tokio::test]
async fn a_retry_resends_when_the_lookup_finds_nothing_and_a_restart_checks_first() {
    let h = Harness::new().await;
    let (_, row) = queued_draft(&h, 4, vec![]).await;
    h.mock
        .fail_next(Call::Send, ProviderError::transient("down"));
    h.step().await;
    let pending = h.row(row).await.unwrap();
    h.advance_past(pending.not_before).await;
    h.step().await;
    assert_eq!(sends(&h), 2, "resent with the same Message-ID");
    assert!(h.row(row).await.is_none());
    // A row found in flight at start: the worker checks before sending.
    let (_, row) = queued_draft(&h, 5, vec![]).await;
    h.claim(row).await;
    h.mock.with(|m| {
        m.known_header_ids.insert("<d5@example.com>".into());
    });
    let worker = h.fresh_worker();
    worker.recover_and_step().await;
    assert_eq!(sends(&h), 2, "not sent again");
    assert!(h.row(row).await.is_none());
}

#[tokio::test]
async fn a_first_attempt_sends_without_a_lookup() {
    let h = Harness::new().await;
    queued_draft(&h, 11, vec![]).await;
    h.step().await;
    assert_eq!(sends(&h), 1);
    assert!(!h.mock.calls().contains(&Call::FindByHeader));
}

#[tokio::test]
async fn a_permanent_refusal_returns_the_draft_to_editing_and_notifies_with_the_draft_and_token() {
    let h = Harness::new().await;
    let (draft, row) = queued_draft(&h, 6, vec![]).await;
    h.mock.fail_next(
        Call::Send,
        ProviderError::permanent("Gmail refused the request (HTTP 400)")
            .with_detail("Invalid To header".into()),
    );
    h.step().await;
    assert_eq!(
        h.with_store(|s| s.draft(draft).unwrap().unwrap().state),
        DraftState::Editing
    );
    let failed = h.failures();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].row, row);
    assert_eq!(failed[0].draft, Some(draft));
    assert_eq!(failed[0].client_token, "tok-6");
    assert!(failed[0].threads.is_empty());
    assert_eq!(
        failed[0].error.detail.as_ref().map(|d| d.get().as_str()),
        Some("Invalid To header")
    );
    assert_eq!(h.sync_wakes(), 0, "nothing landed");
}

#[tokio::test]
async fn a_vanished_attachment_fails_the_send_permanently() {
    let h = Harness::new().await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gone.pdf");
    std::fs::write(&path, b"x").unwrap();
    let (draft, _) = queued_draft(
        &h,
        7,
        vec![Attachment::File {
            path: path.to_string_lossy().into_owned(),
            name: "gone.pdf".into(),
            mime: "application/pdf".into(),
            size: 1,
        }],
    )
    .await;
    std::fs::remove_file(&path).unwrap();
    h.step().await;
    assert_eq!(
        h.with_store(|s| s.draft(draft).unwrap().unwrap().state),
        DraftState::Editing
    );
    let failed = h.failures();
    assert!(
        failed[0].error.message.contains("gone.pdf"),
        "{}",
        failed[0].error.message
    );
    assert!(h.mock.with(|m| m.sent.is_empty()));
}

#[tokio::test]
async fn a_forwarded_part_is_fetched_and_attached() {
    let h = Harness::new().await;
    h.mock.with(|m| {
        m.attachment_bytes
            .insert(("m1".into(), "p1".into()), vec![9, 9, 9]);
    });
    let (_, _) = queued_draft(
        &h,
        8,
        vec![Attachment::Part {
            message: h.message_id("m1"),
            part: "p1".into(),
            name: "orig.pdf".into(),
            mime: "application/pdf".into(),
            size: 3,
        }],
    )
    .await;
    h.step().await;
    assert!(h.mock.calls().contains(&Call::Attachment));
    let raw = String::from_utf8_lossy(&h.mock.with(|m| m.sent[0].raw.clone())).into_owned();
    assert!(raw.contains("orig.pdf"), "{raw}");
}

#[tokio::test]
async fn a_part_whose_message_is_gone_fails_the_send_permanently() {
    let h = Harness::new().await;
    let (draft, _) = queued_draft(
        &h,
        12,
        vec![Attachment::Part {
            message: 9_999,
            part: "p1".into(),
            name: "orig.pdf".into(),
            mime: "application/pdf".into(),
            size: 3,
        }],
    )
    .await;
    h.step().await;
    assert_eq!(
        h.with_store(|s| s.draft(draft).unwrap().unwrap().state),
        DraftState::Editing
    );
    assert!(h.failures()[0].error.message.contains("orig.pdf"));
    assert!(!h.mock.calls().contains(&Call::Attachment));
    assert!(h.mock.with(|m| m.sent.is_empty()));
}

/// Carried from the Task 1 review: an unreadable attachment list is not "no attachments".
#[tokio::test]
async fn a_draft_whose_attachments_cannot_be_read_is_not_sent_without_them() {
    let h = Harness::new().await;
    let (draft, _) = queued_draft(&h, 13, vec![]).await;
    h.with_store(|s| {
        s.conn()
            .execute(
                "UPDATE drafts SET attachments_json = 'not json' WHERE id = ?1",
                [draft],
            )
            .unwrap();
    });
    h.step().await;
    assert!(h.mock.with(|m| m.sent.is_empty()));
    assert_eq!(
        h.failures()[0].error.message,
        "the draft's attachments could not be read"
    );
    assert_eq!(
        h.with_store(|s| s.draft(draft).unwrap().unwrap().state),
        DraftState::Editing
    );
}

#[tokio::test]
async fn a_send_still_goes_out_when_the_replied_message_is_gone() {
    let h = Harness::new().await;
    let (draft, _) = queued_draft(&h, 9, vec![]).await;
    h.with_store(|s| {
        s.conn().execute("UPDATE drafts SET kind = 'reply', remote_thread_id = 't-gone', in_reply_to = '<gone@example.com>', thread_id = NULL WHERE id = ?1", [draft]).unwrap();
    });
    h.step().await;
    let sent = h.mock.with(|m| m.sent.clone());
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].thread_id.as_deref(),
        Some("t-gone"),
        "the saved thread id is used as is"
    );
    let raw = String::from_utf8_lossy(&sent[0].raw);
    assert!(raw.contains("In-Reply-To: <gone@example.com>"), "{raw}");
}

/// Final fix 4: Gmail answers 404 for a thread deleted there; the reply goes out once, on a
/// thread of its own.
#[tokio::test]
async fn a_reply_whose_thread_gmail_no_longer_has_is_sent_once_without_it() {
    let h = Harness::new().await;
    let (draft, _) = queued_draft(&h, 10, vec![]).await;
    h.with_store(|s| {
        s.conn().execute("UPDATE drafts SET kind = 'reply', remote_thread_id = 't-deleted', in_reply_to = '<m1@example.com>' WHERE id = ?1", [draft]).unwrap();
    });
    h.mock
        .with(|m| m.missing_threads.insert("t-deleted".into()));
    h.step().await;
    assert_eq!(
        sends(&h),
        2,
        "refused with the thread, then sent without it"
    );
    let sent = h.mock.with(|m| m.sent.clone());
    assert_eq!(sent.len(), 1, "delivered once");
    assert_eq!(sent[0].thread_id, None);
    let raw = String::from_utf8_lossy(&sent[0].raw);
    assert!(raw.contains("In-Reply-To: <m1@example.com>"), "{raw}");
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_none(), "sent");
}

#[tokio::test]
async fn sending_shows_in_the_depth_while_a_send_is_in_flight() {
    let h = Harness::new().await;
    h.mock.with(|m| m.action_delay = Duration::from_millis(200));
    let (_, _) = queued_draft(&h, 10, vec![]).await;
    let step = tokio::spawn(h.worker_step());
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(h.depth().await.sending, 1);
    step.await.unwrap();
    assert_eq!(h.depth().await.sending, 0);
}

#[tokio::test]
async fn a_draft_push_row_fails_until_a_later_version() {
    let h = Harness::new().await;
    let draft = new_draft(&h, 14, vec![]);
    h.enqueue(NewAction {
        account_id: h.account_id,
        action: Action::PushDraft { draft },
        threads: vec![],
        not_before: 0,
        client_token: "push".into(),
    })
    .await;
    h.step().await;
    let failed = h.failures();
    assert_eq!(
        failed[0].error.message,
        "saving drafts to Gmail arrives in a later version"
    );
    assert_eq!(failed[0].draft, Some(draft));
    assert!(h.mock.calls().is_empty(), "no Gmail call");
}

#[tokio::test]
async fn shutdown_waits_for_a_send_in_flight() {
    let mut h = Harness::new().await;
    h.mock.with(|m| m.action_delay = Duration::from_millis(300));
    let (draft, row) = queued_draft(&h, 16, vec![]).await;
    h.rig.start();
    h.rig
        .until("sending", |r| r.mock.calls().contains(&Call::Send))
        .await;
    h.rig.stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), h.rig.task.take().unwrap())
        .await
        .expect("stopped in time")
        .unwrap();
    assert_eq!(h.mock.with(|m| m.sent.len()), 1);
    assert!(h.row(row).await.is_none(), "recorded as sent");
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_none());
}

/// Fix round 1: the stamp makes the check survive a retry that resets the attempt count. The
/// 20th transient attempt of a send that did reach Gmail gives up; the user's retry looks the
/// message up and does not send it again.
#[tokio::test]
async fn a_retry_after_giving_up_looks_the_message_up_and_does_not_send_twice() {
    let h = Harness::new().await;
    let (draft, row) = queued_draft(&h, 17, vec![]).await;
    h.with_store(|s| {
        s.conn()
            .execute("UPDATE outbox SET attempts = 19 WHERE id = ?1", [row])
            .unwrap();
    });
    h.mock
        .fail_next(Call::Send, ProviderError::transient("timed out"));
    h.step().await;
    assert_eq!(h.failures().len(), 1, "gave up");
    assert!(
        h.with_store(|s| s.draft(draft).unwrap().unwrap().send_attempted_at)
            .is_some(),
        "stamped before the send call, kept through the failure"
    );
    let retried = h
        .rig
        .writer
        .outbox(OutboxWrite::Retry {
            id: row,
            client_token: "tok-17".into(),
        })
        .await
        .unwrap();
    assert_eq!(retried, OutboxOutcome::Retried(true));
    assert_eq!(h.row(row).await.unwrap().attempts, 0);
    // Gmail did get it.
    h.mock.with(|m| {
        m.known_header_ids.insert("<d17@example.com>".into());
    });
    h.step().await;
    let calls = h.mock.calls();
    let looked = calls.iter().position(|c| *c == Call::FindByHeader);
    assert!(looked.is_some(), "{calls:?}");
    assert_eq!(sends(&h), 1, "never sent a second time");
    assert!(h.row(row).await.is_none(), "completed as sent");
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_none());
}

/// Fix round 1: a permanent failure after Gmail accepted the message, then a new `Send` row for
/// the same draft (a new token, attempts 0): the lookup runs first.
#[tokio::test]
async fn a_new_send_row_after_a_permanent_failure_looks_the_message_up_first() {
    let h = Harness::new().await;
    let (draft, first) = queued_draft(&h, 18, vec![]).await;
    h.mock
        .fail_next(Call::Send, ProviderError::permanent("Gmail refused"));
    h.step().await;
    assert_eq!(
        h.with_store(|s| s.draft(draft).unwrap().unwrap().state),
        DraftState::Editing
    );
    h.mock.with(|m| {
        m.known_header_ids.insert("<d18@example.com>".into());
    });
    let again = h
        .enqueue(NewAction {
            account_id: h.account_id,
            action: Action::Send { draft },
            threads: vec![],
            not_before: 0,
            client_token: "tok-18-again".into(),
        })
        .await;
    assert_ne!(again, first);
    h.step().await;
    assert!(h.mock.calls().contains(&Call::FindByHeader));
    assert_eq!(sends(&h), 1, "never sent a second time");
    assert!(h.row(again).await.is_none());
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_none());
}

/// Fix round 1: Gmail answers a send with a 2xx Rail cannot read: still sent, the row completes
/// and the draft is gone (no failure for the user to retry into a second send).
#[tokio::test]
async fn a_2xx_with_an_unreadable_answer_completes_the_send() {
    use crate::provider::gmail::fixtures::{API_PATH, Gmail};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, ResponseTemplate};
    let h = Harness::new().await;
    let gmail = Gmail::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/messages/send")))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"nope": 1})))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let (draft, row) = queued_draft(&h, 19, vec![]).await;
    let sync_wake = Arc::new(Notify::new());
    let worker = h.rig.worker_on(Arc::new(gmail.provider()), None, sync_wake);
    worker.step().await;
    assert!(h.row(row).await.is_none(), "completed");
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_none());
    assert!(h.failures().is_empty());
}
