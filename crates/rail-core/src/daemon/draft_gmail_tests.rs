//! `draft.push`, `draft.import` and the discard of a pushed draft through a real daemon over
//! the fake Gmail, on Part A's harness: the grace and its undo, a second push folding into the
//! first, the refusals, an import with and without its body (and one whose fetch fails), the
//! import's refusals, a token another draft used, and a
//! discard that deletes the Gmail copy and waits for a push in flight.

use std::time::Duration;

use super::draft_tests::{BOUND, DaemonHarness, WORK, save_request};
use crate::error::ErrorClass;
use crate::ops::op::{ActionUndo, DraftDiscard, DraftImport, DraftPush, DraftSave, Operation};
use crate::ops::{
    Action, ActionUndoRequest, DraftDiscardRequest, DraftId, DraftImportRequest, DraftPushRequest,
    DraftSaveRequest, ErrorCode, OpError, RowToken, names,
};
use crate::paths::Paths;
use crate::provider::mock::{Call, MockDraft, MockProvider, envelope};
use crate::provider::{AttachmentMeta, Body, Envelope, Mailbox, ProviderError};
use crate::store::Store;
use crate::store::drafts::{Attachment, DraftKind, DraftWrite, Recipient};
use crate::store::outbox::OutboxState;

/// The bytes of d1's attachment `p1` at Gmail.
pub(super) const PLAN: &[u8] = b"%PDF-1.4 the plan";

fn mailbox(address: &str) -> Mailbox {
    Mailbox {
        name: None,
        address: address.into(),
    }
}

/// d1: a Gmail draft answering m1 in t1, with Cc, Bcc and one attachment; its Gmail draft is
/// r1. d3: a draft message no Gmail draft names any more.
pub(super) fn seed_drafts(mock: &MockProvider) {
    let now = crate::auth::unix_now() * 1000;
    mock.seed(Envelope {
        message_id: Some("m1@example.com".into()),
        references: vec!["m0@example.com".into()],
        ..envelope("m1", "t1", now - 1_000)
    });
    mock.seed(Envelope {
        message_id: Some("<d1@example.com>".into()),
        in_reply_to: Some("m1@example.com".into()),
        references: vec!["m0@example.com".into(), "<m1@example.com>".into()],
        subject: "Re: plan".into(),
        to: vec![mailbox("sam@example.com")],
        cc: vec![mailbox("kim@example.com")],
        bcc: vec![mailbox("lee@example.com")],
        is_draft: true,
        folders: vec!["DRAFT".into()],
        ..envelope("d1", "t1", now - 500)
    });
    mock.seed(Envelope {
        is_draft: true,
        folders: vec!["DRAFT".into()],
        ..envelope("d3", "t3", now - 700)
    });
    mock.with(|m| {
        m.bodies.insert(
            "d1".into(),
            Body {
                text: "the draft's text".into(),
                attachments: vec![AttachmentMeta {
                    part_id: "p1".into(),
                    filename: "plan.pdf".into(),
                    mime_type: "application/pdf".into(),
                    size: 1234,
                }],
                ..Body::default()
            },
        );
        m.drafts.insert(
            "r1".into(),
            MockDraft {
                message_id: "d1".into(),
                raw: Vec::new(),
                thread_id: Some("t1".into()),
            },
        );
        m.attachment_bytes
            .insert(("d1".into(), "p1".into()), PLAN.to_vec());
        // The worker's creates are r2 onwards.
        m.next_draft = 1;
    });
}

pub(super) async fn with_gmail_drafts() -> DaemonHarness {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let mock = MockProvider::new();
    seed_drafts(&mock);
    DaemonHarness::boot(tmp, paths, mock).await
}

impl DaemonHarness {
    pub(super) async fn push(&self, draft: DraftId, token: &str) -> Result<i64, OpError> {
        let request = DraftPushRequest {
            draft,
            client_token: token.into(),
        };
        Ok(self.call::<DraftPush>(&request).await?.row)
    }

    pub(super) async fn import(&self, message: i64, token: &str) -> Result<DraftId, OpError> {
        let request = DraftImportRequest {
            account: "work".into(),
            message,
            client_token: token.into(),
        };
        Ok(self.call::<DraftImport>(&request).await?.draft)
    }

    pub(super) async fn discard(&self, draft: DraftId) -> Result<(), OpError> {
        self.call::<DraftDiscard>(&DraftDiscardRequest { draft })
            .await
            .map(|_| ())
    }

    pub(super) async fn saved(&self, token: &str) -> DraftId {
        self.call::<DraftSave>(&save_request(None, token))
            .await
            .unwrap()
            .draft
    }

    pub(super) async fn undo(&self, row: i64, token: &str) -> crate::ops::ActionUndoResponse {
        let request = ActionUndoRequest {
            rows: vec![RowToken {
                row,
                client_token: token.into(),
            }],
        };
        self.call::<ActionUndo>(&request).await.unwrap()
    }

    /// Make `row` due now and wake the worker (a discard of no draft signals the outbox).
    pub(super) async fn due_now(&self, row: i64) {
        Store::open(&self.paths.db_file())
            .unwrap()
            .conn()
            .execute("UPDATE outbox SET not_before = 0 WHERE id = ?1", [row])
            .unwrap();
        self.discard(999_999).await.unwrap();
    }

    pub(super) async fn until(&self, what: &str, done: impl Fn(&Self) -> bool) {
        tokio::time::timeout(BOUND, async {
            while !done(self) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what}"));
    }

    pub(super) fn work_id(&self) -> i64 {
        self.store(|s| s.list_accounts().unwrap()[0].id)
    }

    pub(super) fn body_calls(&self) -> usize {
        self.mock
            .calls()
            .into_iter()
            .filter(|c| *c == Call::Body)
            .count()
    }

    pub(super) fn drafts_count(&self) -> i64 {
        self.store(|s| {
            s.conn()
                .query_row("SELECT count(*) FROM drafts", [], |r| r.get(0))
                .unwrap()
        })
    }
}

#[tokio::test]
async fn a_push_queues_one_row_that_undo_cancels() {
    let h = DaemonHarness::new().await;
    let draft = h.saved("s1").await;
    let row = h.push(draft, "p1").await.unwrap();
    let pending = h.store(|s| s.pending_rows_for_draft(draft).unwrap());
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, row);
    assert_eq!(pending[0].action, Action::PushDraft { draft });
    assert!(pending[0].not_before >= h.now() + 9, "the undo grace");
    let undo = h.undo(row, "p1").await;
    assert_eq!(undo.undone, [row]);
    assert!(
        h.store(|s| s.pending_rows_for_draft(draft).unwrap())
            .is_empty()
    );
    assert!(h.store(|s| s.draft(draft).unwrap()).is_some(), "kept");
    assert!(
        h.mock.with(|m| m.drafts.is_empty()),
        "nothing reached Gmail"
    );
}

/// Review Focus 2: `s` twice inside the grace is one row, and the newest token owns it.
#[tokio::test]
async fn pushing_twice_queues_one_row() {
    let h = DaemonHarness::new().await;
    let draft = h.saved("s1").await;
    let first = h.push(draft, "p1").await.unwrap();
    let second = h.push(draft, "p2").await.unwrap();
    assert_eq!(second, first, "the second push answers the pending row");
    assert_eq!(h.push(draft, "p2").await.unwrap(), first, "a replay too");
    let pending = h.store(|s| s.pending_rows_for_draft(draft).unwrap());
    assert_eq!(pending.len(), 1, "one row however often it is pushed");
    assert_eq!(h.undo(first, "p1").await.too_late, [first], "an old token");
    assert_eq!(h.undo(first, "p2").await.undone, [first]);
    assert!(
        h.store(|s| s.pending_rows_for_draft(draft).unwrap())
            .is_empty()
    );
}

#[tokio::test]
async fn a_push_is_refused_for_a_draft_being_sent_an_empty_draft_and_another_account() {
    let h = DaemonHarness::new().await;
    let sending = h.saved("s1").await;
    h.send(sending, "send-1").await.unwrap();
    let error = h.push(sending, "p1").await.unwrap_err();
    assert_eq!(error.message, "draft: this draft is being sent; undo first");
    let empty = DraftSaveRequest {
        to: vec![],
        subject: String::new(),
        body: " \n ".into(),
        ..save_request(None, "s2")
    };
    let empty = h.call::<DraftSave>(&empty).await.unwrap().draft;
    let error = h.push(empty, "p2").await.unwrap_err();
    assert_eq!(error.message, "draft: there is nothing to save");
    let error = h.push(999_999, "p3").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{}", error.message);
    assert_eq!(error.message, "draft: no such draft");
    let error = h.push(0, "p3").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput, "{}", error.message);
    // A draft of an account in the store that is not configured: refused, nothing queued.
    let other = {
        let mut store = Store::open(&h.paths.db_file()).unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO accounts (name, kind, address) VALUES ('home', 'gmail', 'h@example.com')",
                [],
            )
            .unwrap();
        let account_id = store.conn().last_insert_rowid();
        let write = DraftWrite {
            id: None,
            account_id,
            kind: DraftKind::New,
            thread_id: None,
            remote_thread_id: None,
            in_reply_to: None,
            references: None,
            to: vec![Recipient {
                name: None,
                address: "sam@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "s".into(),
            body: "b".into(),
            attachments: vec![],
            message_id: "<o1@example.com>".into(),
        };
        store.draft_upsert(&write, h.now()).unwrap().unwrap()
    };
    let error = h.push(other, "p4").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "{}", error.message);
    for draft in [sending, empty, other] {
        let rows = h.store(|s| s.pending_rows_for_draft(draft).unwrap());
        assert!(rows.iter().all(|r| r.action != Action::PushDraft { draft }));
    }
}

#[tokio::test]
async fn an_import_with_its_body_stored_makes_a_draft_with_its_gmail_identity() {
    let h = with_gmail_drafts().await;
    let message = h.message_id("d1");
    let source = h.store(|s| s.import_source(message).unwrap().unwrap());
    assert!(source.body_text.is_some(), "the sync stored the body");
    let fetches = h.body_calls();
    let id = h.import(message, "i1").await.unwrap();
    assert_eq!(h.body_calls(), fetches, "no fetch for a stored body");
    let d = h.store(|s| s.draft(id).unwrap().unwrap());
    assert_eq!(d.kind, DraftKind::Reply);
    assert_eq!(d.account_id, h.work_id());
    let addresses = |r: &[Recipient]| r.iter().map(|r| r.address.clone()).collect::<Vec<_>>();
    assert_eq!(addresses(&d.to), ["sam@example.com"]);
    assert_eq!(addresses(&d.cc), ["kim@example.com"]);
    assert_eq!(addresses(&d.bcc), ["lee@example.com"]);
    assert_eq!(d.subject, "Re: plan");
    assert_eq!(d.body, "the draft's text");
    let path = h.paths.draft_files_dir().join("i1").join("plan.pdf");
    assert_eq!(
        d.attachments,
        [Attachment::File {
            path: path.to_str().unwrap().into(),
            name: "plan.pdf".into(),
            mime: "application/pdf".into(),
            size: i64::try_from(PLAN.len()).unwrap(),
        }]
    );
    assert_eq!(d.thread_id, Some(source.thread_id));
    assert_eq!(d.remote_thread_id.as_deref(), Some("t1"));
    assert_eq!(d.in_reply_to.as_deref(), Some("<m1@example.com>"));
    assert_eq!(
        d.references.as_deref(),
        Some("<m0@example.com> <m1@example.com>")
    );
    assert_eq!(d.remote_draft_id.as_deref(), Some("r1"));
    assert_eq!(d.remote_message_id.as_deref(), Some("d1"));
    assert_eq!(d.message_id, "<d1@example.com>");
}

#[tokio::test]
async fn an_import_without_its_body_fetches_it_first() {
    let h = with_gmail_drafts().await;
    let message = h.message_id("d1");
    Store::open(&h.paths.db_file())
        .unwrap()
        .conn()
        .execute_batch(&format!(
            "UPDATE messages SET body_text = NULL, body_fetched_at = NULL WHERE id = {message};
             DELETE FROM attachments WHERE message_id = {message};"
        ))
        .unwrap();
    h.mock
        .with(|m| m.bodies.get_mut("d1").unwrap().text = "fetched now".into());
    let fetches = h.body_calls();
    let id = h.import(message, "i1").await.unwrap();
    assert_eq!(h.body_calls(), fetches + 1, "the body was fetched");
    let d = h.store(|s| s.draft(id).unwrap().unwrap());
    assert_eq!(d.body, "fetched now");
    assert_eq!(d.attachments.len(), 1, "the fetch stored the attachment");
}

/// A body fetch that fails fails the import: nothing is written.
#[tokio::test]
async fn an_import_whose_body_fetch_fails_says_why_and_writes_nothing() {
    let h = with_gmail_drafts().await;
    let message = h.message_id("d1");
    Store::open(&h.paths.db_file())
        .unwrap()
        .conn()
        .execute(
            "UPDATE messages SET body_text = NULL, body_fetched_at = NULL WHERE id = ?1",
            [message],
        )
        .unwrap();
    h.mock
        .fail_next(Call::Body, ProviderError::transient("Gmail is busy"));
    let error = h.import(message, "i1").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Network, "{}", error.message);
    assert_eq!(h.drafts_count(), 0);
    assert!(!h.paths.draft_files_dir().join("i1").exists());
}

/// A token `draft.save` used answers that draft; an import with it is refused.
#[tokio::test]
async fn an_import_with_a_token_a_save_used_is_refused() {
    let h = with_gmail_drafts().await;
    let saved = h.saved("t1").await;
    let error = h.import(h.message_id("d1"), "t1").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert_eq!(
        error.message,
        "client_token: this token was used for another action"
    );
    let d = h.store(|s| s.draft(saved).unwrap().unwrap());
    assert_eq!((d.remote_draft_id, d.remote_message_id), (None, None));
    assert_eq!(h.drafts_count(), 1);
    assert!(!h.paths.draft_files_dir().join("t1").exists());
}

#[tokio::test]
async fn importing_twice_answers_one_draft() {
    let h = with_gmail_drafts().await;
    let message = h.message_id("d1");
    let first = h.import(message, "i1").await.unwrap();
    let second = h.import(message, "i2").await.unwrap();
    assert_eq!(second, first);
    assert_eq!(h.import(message, "i1").await.unwrap(), first, "a replay");
    assert_eq!(h.drafts_count(), 1);
}

#[tokio::test]
async fn an_import_is_refused_for_a_message_that_is_not_a_draft_or_not_this_accounts() {
    let h = with_gmail_drafts().await;
    let error = h.import(h.message_id("m1"), "i1").await.unwrap_err();
    assert_eq!(error.message, "message: not a draft");
    let foreign = {
        let mut store = Store::open(&h.paths.db_file()).unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO accounts (name, kind, address) VALUES ('home', 'gmail', 'h@example.com')",
                [],
            )
            .unwrap();
        let account_id = store.conn().last_insert_rowid();
        let batch = crate::sync::batch::Batch {
            upserts: vec![Envelope {
                is_draft: true,
                ..envelope("h1", "th1", h.now() * 1000)
            }],
            ..crate::sync::batch::Batch::new(account_id)
        };
        store.apply(&batch, h.now()).unwrap();
        store
            .conn()
            .query_row("SELECT id FROM messages WHERE remote_id = 'h1'", [], |r| {
                r.get(0)
            })
            .unwrap()
    };
    let error = h.import(foreign, "i2").await.unwrap_err();
    assert_eq!(error.message, "message: not a message of this account");
    for missing in [0, -4, 999_999] {
        let error = h.import(missing, "i3").await.unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound, "{}", error.message);
        assert_eq!(error.message, "message: no such message");
    }
    assert_eq!(h.drafts_count(), 0);
}

#[tokio::test]
async fn an_import_gmail_no_longer_has_says_so() {
    let h = with_gmail_drafts().await;
    let error = h.import(h.message_id("d3"), "i1").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.message, "Gmail no longer has this draft");
    assert_eq!(h.drafts_count(), 0, "no local draft");
}

/// Review Focus 3, through the daemon.
#[tokio::test]
async fn discarding_a_pushed_draft_deletes_it_at_gmail_and_waits_for_a_push_in_flight() {
    let h = with_gmail_drafts().await;
    let imported = h.import(h.message_id("d1"), "i1").await.unwrap();
    h.discard(imported).await.unwrap();
    assert!(h.store(|s| s.draft(imported).unwrap()).is_none());
    h.until("Gmail's r1 deleted", |h| {
        h.mock.with(|m| !m.drafts.contains_key("r1"))
    })
    .await;

    let draft = h.saved("s1").await;
    h.mock
        .with(|m| m.action_delay = Duration::from_millis(1_500));
    let row = h.push(draft, "p1").await.unwrap();
    h.due_now(row).await;
    h.until("the push in flight", |h| {
        h.store(|s| s.pending_rows_for_draft(draft).unwrap())
            .first()
            .is_some_and(|r| r.state == OutboxState::Inflight)
    })
    .await;
    let error = h.discard(draft).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert_eq!(error.class, ErrorClass::Transient);
    assert_eq!(
        error.message,
        "draft: saving to Gmail, try again in a moment"
    );
    assert!(h.store(|s| s.draft(draft).unwrap()).is_some(), "kept");
    h.until("the push landed", |h| {
        h.store(|s| s.pending_rows_for_draft(draft).unwrap())
            .is_empty()
    })
    .await;
    h.mock.with(|m| m.action_delay = Duration::ZERO);
    let pushed = h.store(|s| s.draft(draft).unwrap().unwrap().remote_draft_id);
    assert_eq!(pushed.as_deref(), Some("r2"));
    h.discard(draft).await.unwrap();
    assert!(h.store(|s| s.draft(draft).unwrap()).is_none());
    h.until("Gmail's r2 deleted", |h| {
        h.mock.with(|m| m.drafts.is_empty())
    })
    .await;
}

#[test]
fn the_new_ops_are_terminal_only() {
    // No MCP tool may carry `draft` in its name or `client_token` in its schema
    // (`crates/rail/tests/mcp.rs`); both new operations do, so no tool can stand for them.
    for (name, request) in [
        (DraftPush::NAME, schemars::schema_for!(DraftPushRequest)),
        (DraftImport::NAME, schemars::schema_for!(DraftImportRequest)),
    ] {
        assert!(names::ALL.contains(&name), "{name}");
        assert!(name.starts_with("draft."), "{name}");
        let schema = serde_json::to_string(&request).unwrap();
        assert!(schema.contains("client_token"), "{schema}");
        assert!(
            crate::daemon::retry_safe(name),
            "{name}: the token makes it safe"
        );
    }
    assert_eq!(names::DRAFT_PUSH, "draft.push");
    assert_eq!(names::DRAFT_IMPORT, "draft.import");
}
