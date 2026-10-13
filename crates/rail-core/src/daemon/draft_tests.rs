//! `draft.save`, `draft.send` and `draft.discard` through a real daemon over the fake Gmail:
//! create, update and replay, threading from the store, the file checks, the grace, the
//! refusals while a draft is being sent, and an undo by token across a daemon restart.

use std::sync::Arc;
use std::time::Duration;

use super::client::DaemonClient;
use super::{Connector, ServeConfig};
use crate::ops::op::{ActionUndo, DaemonStop, DraftDiscard, DraftSave, DraftSend, SyncStatus};
use crate::ops::{
    ActionUndoRequest, DaemonStopRequest, DraftAttachment, DraftDiscardRequest, DraftId,
    DraftSaveRequest, DraftSendRequest, ErrorCode, OpError, Operation, ReplyTo, RowToken,
    SyncStatusRequest,
};
use crate::paths::Paths;
use crate::provider::Envelope;
use crate::provider::mock::{MockProvider, envelope};
use crate::store::Store;
use crate::store::drafts::{Attachment, DraftKind, DraftState};
use crate::test_support::{daemon_context, mock_factory, start_daemon};

pub(super) const WORK: &str = "[general]\nsecrets = \"file\"\npoll_interval = \"60s\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";
pub(super) const BOUND: Duration = Duration::from_secs(15);

type DaemonTask = tokio::task::JoinHandle<Result<(), OpError>>;

pub(super) struct DaemonHarness {
    tmp: tempfile::TempDir,
    pub(super) paths: Paths,
    pub(super) mock: MockProvider,
    pub(super) client: DaemonClient,
    task: DaemonTask,
}

impl DaemonHarness {
    /// A synced daemon whose store holds m1 (thread t1), m2 and m3.
    pub(super) async fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(tmp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), WORK).unwrap();
        let mock = MockProvider::new();
        let now = crate::auth::unix_now() * 1000;
        // Ids as Gmail's metadata stores them: bare. m2's are bracketed and one reference is
        // bad; m3's own id is bad.
        mock.seed(Envelope {
            message_id: Some("m1@example.com".into()),
            references: vec!["m0@example.com".into()],
            ..envelope("m1", "t1", now - 1_000)
        });
        mock.seed(Envelope {
            message_id: Some("<m2@example.com>".into()),
            references: vec!["<bad id@example.com>".into(), "<m1@example.com>".into()],
            ..envelope("m2", "t2", now - 2_000)
        });
        mock.seed(Envelope {
            message_id: Some("m3;x@example.com".into()),
            ..envelope("m3", "t3", now - 3_000)
        });
        Self::boot(tmp, paths, mock).await
    }

    /// `new`, with an attachment (part `p1`) stored for m1.
    pub(super) async fn with_messages() -> Self {
        let h = Self::new().await;
        let message = h.message_id("m1");
        Store::open(&h.paths.db_file())
            .unwrap()
            .conn()
            .execute(
                "INSERT INTO attachments (message_id, remote_part_id, filename, mime_type, size)
                 VALUES (?1, 'p1', 'plan.pdf', 'application/pdf', 1234)",
                [message],
            )
            .unwrap();
        h
    }

    pub(super) async fn boot(tmp: tempfile::TempDir, paths: Paths, mock: MockProvider) -> Self {
        let (ctx, _) = daemon_context(&paths);
        let task = start_daemon(
            ctx.with_provider_factory(mock_factory(&mock)),
            ServeConfig::default(),
        )
        .await;
        let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
            .connect()
            .await
            .unwrap();
        let h = Self {
            tmp,
            paths,
            mock,
            client,
            task,
        };
        h.until_synced().await;
        h
    }

    /// Stop the daemon, wait for it to end, and start another over the same files.
    pub(super) async fn restart(self) -> Self {
        self.client
            .call::<DaemonStop>(&DaemonStopRequest {})
            .await
            .unwrap();
        tokio::time::timeout(BOUND, self.task)
            .await
            .expect("the daemon stops")
            .unwrap()
            .unwrap();
        Self::boot(self.tmp, self.paths, self.mock).await
    }

    pub(super) async fn until_synced(&self) {
        tokio::time::timeout(BOUND, async {
            loop {
                let request = SyncStatusRequest {
                    account: Some("work".into()),
                };
                if let Ok(s) = self.client.call::<SyncStatus>(&request).await
                    && s.accounts[0].passes_completed > 0
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("synced");
    }

    pub(super) async fn call<O: Operation>(
        &self,
        request: &O::Request,
    ) -> Result<O::Response, OpError> {
        self.client.call::<O>(request).await
    }

    pub(super) fn store<R>(&self, read: impl FnOnce(&Store) -> R) -> R {
        read(&Store::open_reader(&self.paths.db_file()).unwrap())
    }

    pub(super) fn now(&self) -> i64 {
        crate::auth::unix_now()
    }

    /// The store's id of the message Gmail calls `remote`.
    pub(super) fn message_id(&self, remote: &str) -> i64 {
        self.store(|s| {
            s.conn()
                .query_row(
                    "SELECT id FROM messages WHERE remote_id = ?1",
                    [remote],
                    |r| r.get(0),
                )
                .unwrap()
        })
    }

    pub(super) async fn send(&self, draft: DraftId, token: &str) -> Result<i64, OpError> {
        let request = DraftSendRequest {
            draft,
            client_token: token.into(),
        };
        Ok(self.call::<DraftSend>(&request).await?.row)
    }
}

type Headers = (Option<String>, Option<String>, Option<String>);

impl DaemonHarness {
    /// Save a reply to `remote`: the draft's In-Reply-To, References and thread.
    async fn reply_headers(&self, remote: &str, token: &str) -> Headers {
        let request = DraftSaveRequest {
            kind: DraftKind::Reply,
            reply_to: Some(ReplyTo {
                message: self.message_id(remote),
            }),
            ..save_request(None, token)
        };
        let saved = self.call::<DraftSave>(&request).await.unwrap();
        let d = self.store(|s| s.draft(saved.draft).unwrap().unwrap());
        (d.in_reply_to, d.references, d.remote_thread_id)
    }
}

/// A new draft for `work` to sam@example.com.
pub(super) fn save_request(draft: Option<DraftId>, token: &str) -> DraftSaveRequest {
    DraftSaveRequest {
        account: "work".into(),
        draft,
        kind: DraftKind::New,
        reply_to: None,
        to: vec!["sam@example.com".into()],
        cc: vec![],
        bcc: vec![],
        subject: "Hello".into(),
        body: "hi".into(),
        attachments: vec![],
        client_token: token.into(),
    }
}

#[tokio::test]
async fn save_creates_then_updates_and_send_queues_with_the_grace() {
    let h = DaemonHarness::new().await;
    let first = h
        .call::<DraftSave>(&save_request(None, "tok-1"))
        .await
        .unwrap();
    assert!(first.message_id.ends_with("@example.com>"));
    let edited = DraftSaveRequest {
        draft: Some(first.draft),
        subject: "Edited".into(),
        ..save_request(None, "tok-2")
    };
    let again = h.call::<DraftSave>(&edited).await.unwrap();
    assert_eq!(again.draft, first.draft);
    assert_eq!(again.message_id, first.message_id);
    let replay = h
        .call::<DraftSave>(&save_request(None, "tok-1"))
        .await
        .unwrap();
    assert_eq!(
        replay.draft, first.draft,
        "the same token answers the same draft"
    );
    let draft = h.store(|s| s.draft(first.draft).unwrap().unwrap());
    assert_eq!(draft.subject, "Edited", "a replayed create changes nothing");
    let row = h.send(first.draft, "send-1").await.unwrap();
    assert!(h.store(|s| s.failed_rows(1).unwrap()).is_empty());
    let pending = h.store(|s| s.pending_rows_for_draft(first.draft).unwrap());
    assert_eq!(pending[0].id, row);
    assert!(pending[0].not_before >= h.now() + 9);
    let twice = h.send(first.draft, "send-2").await.unwrap_err();
    assert!(
        twice.message.contains("already being sent"),
        "{}",
        twice.message
    );
    let discard = h
        .call::<DraftDiscard>(&DraftDiscardRequest { draft: first.draft })
        .await
        .unwrap_err();
    assert!(
        discard.message.contains("undo first"),
        "{}",
        discard.message
    );
    let update = h.call::<DraftSave>(&edited).await.unwrap_err();
    assert!(update.message.contains("undo first"), "{}", update.message);
}

#[tokio::test]
async fn a_send_replayed_with_its_token_answers_the_same_row() {
    let h = DaemonHarness::new().await;
    let saved = h
        .call::<DraftSave>(&save_request(None, "s1"))
        .await
        .unwrap();
    let row = h.send(saved.draft, "send-1").await.unwrap();
    assert_eq!(h.send(saved.draft, "send-1").await.unwrap(), row);
    let rows = h.store(|s| s.pending_rows_for_draft(saved.draft).unwrap());
    assert_eq!(rows.len(), 1, "one row however often it is sent");
}

#[tokio::test]
async fn a_token_another_drafts_send_used_is_refused() {
    let h = DaemonHarness::new().await;
    let a = h
        .call::<DraftSave>(&save_request(None, "da"))
        .await
        .unwrap();
    let b = h
        .call::<DraftSave>(&save_request(None, "db"))
        .await
        .unwrap();
    h.send(a.draft, "tok-a").await.unwrap();
    let error = h.send(b.draft, "tok-a").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert_eq!(
        error.message,
        "client_token: this token was used for another action"
    );
    assert!(
        h.store(|s| s.pending_rows_for_draft(b.draft).unwrap())
            .is_empty()
    );
    let draft = h.store(|s| s.draft(b.draft).unwrap().unwrap());
    assert_eq!(draft.state, DraftState::Editing);
}

#[tokio::test]
async fn a_reply_gets_its_headers_from_the_store_and_a_forward_its_parts() {
    let h = DaemonHarness::with_messages().await;
    let message = h.message_id("m1");
    let reply = DraftSaveRequest {
        kind: DraftKind::Reply,
        reply_to: Some(ReplyTo { message }),
        ..save_request(None, "r1")
    };
    let reply = h.call::<DraftSave>(&reply).await.unwrap();
    let draft = h.store(|s| s.draft(reply.draft).unwrap().unwrap());
    assert_eq!(draft.in_reply_to.as_deref(), Some("<m1@example.com>"));
    assert_eq!(
        draft.references.as_deref(),
        Some("<m0@example.com> <m1@example.com>")
    );
    assert_eq!(draft.remote_thread_id.as_deref(), Some("t1"));
    assert!(draft.attachments.is_empty());
    assert!(reply.attachments.is_empty());
    let forward = DraftSaveRequest {
        kind: DraftKind::Forward,
        reply_to: Some(ReplyTo { message }),
        ..save_request(None, "f1")
    };
    let forward = h.call::<DraftSave>(&forward).await.unwrap();
    let draft = h.store(|s| s.draft(forward.draft).unwrap().unwrap());
    assert!(matches!(draft.attachments[0], Attachment::Part { ref part, .. } if part == "p1"));
    assert_eq!(
        (
            forward.attachments[0].name.as_str(),
            forward.attachments[0].path.as_ref()
        ),
        ("plan.pdf", None)
    );
    let bracketed = h.reply_headers("m2", "r3").await;
    assert_eq!(
        bracketed,
        (
            Some("<m2@example.com>".into()),
            Some("<m1@example.com> <m2@example.com>".into()),
            Some("t2".into())
        ),
        "a bracketed id is kept once and a bad reference is dropped"
    );
    let bad = h.reply_headers("m3", "r4").await;
    assert_eq!(
        bad,
        (None, None, Some("t3".into())),
        "a bad id never blocks a reply"
    );
    let gone = DraftSaveRequest {
        kind: DraftKind::Reply,
        reply_to: Some(ReplyTo { message: 999 }),
        ..save_request(None, "r2")
    };
    let gone = h.call::<DraftSave>(&gone).await.unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);
    assert!(gone.message.starts_with("reply_to:"), "{}", gone.message);
}

#[tokio::test]
async fn an_update_without_reply_to_keeps_the_drafts_threading_and_parts() {
    let h = DaemonHarness::with_messages().await;
    let message = h.message_id("m1");
    let forward = DraftSaveRequest {
        kind: DraftKind::Forward,
        reply_to: Some(ReplyTo { message }),
        ..save_request(None, "f9")
    };
    let made = h.call::<DraftSave>(&forward).await.unwrap();
    let before = h.store(|s| s.draft(made.draft).unwrap().unwrap());
    let update = DraftSaveRequest {
        kind: DraftKind::Forward,
        draft: Some(made.draft),
        reply_to: None,
        body: "edited\n".into(),
        attachments: vec![DraftAttachment::Part {
            message,
            part: "p1".into(),
        }],
        ..save_request(None, "f10")
    };
    let saved = h.call::<DraftSave>(&update).await.unwrap();
    assert_eq!(saved.draft, made.draft);
    let after = h.store(|s| s.draft(made.draft).unwrap().unwrap());
    assert_eq!(after.body, "edited\n");
    assert_eq!(
        (
            after.thread_id,
            &after.remote_thread_id,
            &after.in_reply_to,
            &after.references
        ),
        (
            before.thread_id,
            &before.remote_thread_id,
            &before.in_reply_to,
            &before.references
        )
    );
    assert_eq!(after.in_reply_to.as_deref(), Some("<m1@example.com>"));
    assert_eq!(
        after.attachments, before.attachments,
        "the forwarded part it lists stays"
    );
    let create = DraftSaveRequest {
        kind: DraftKind::Reply,
        reply_to: None,
        ..save_request(None, "r9")
    };
    assert!(
        h.call::<DraftSave>(&create).await.is_err(),
        "a create still names it"
    );
}

/// Final fix 2: an update keeps only the forwarded parts it lists, and a part the draft never
/// had is refused, as is one named by a create.
#[tokio::test]
async fn an_update_keeps_only_the_parts_it_lists_and_refuses_one_never_stored() {
    let h = DaemonHarness::with_messages().await;
    let message = h.message_id("m1");
    Store::open(&h.paths.db_file())
        .unwrap()
        .conn()
        .execute(
            "INSERT INTO attachments (message_id, remote_part_id, filename, mime_type, size)
             VALUES (?1, 'p2', 'notes.txt', 'text/plain', 10)",
            [message],
        )
        .unwrap();
    let forward = DraftSaveRequest {
        kind: DraftKind::Forward,
        reply_to: Some(ReplyTo { message }),
        ..save_request(None, "k1")
    };
    let made = h.call::<DraftSave>(&forward).await.unwrap();
    assert_eq!(made.attachments.len(), 2, "a create takes every part");
    assert_eq!(made.attachments[1].message, Some(message));
    assert_eq!(made.attachments[1].part.as_deref(), Some("p2"));
    let part = |part: &str| DraftAttachment::Part {
        message,
        part: part.into(),
    };
    let update = |attachments, token: &str| DraftSaveRequest {
        kind: DraftKind::Forward,
        draft: Some(made.draft),
        attachments,
        ..save_request(None, token)
    };
    let kept = h
        .call::<DraftSave>(&update(vec![part("p2")], "k2"))
        .await
        .unwrap();
    assert_eq!(kept.attachments.len(), 1);
    assert_eq!(kept.attachments[0].name, "notes.txt");
    let stored = h.store(|s| s.draft(made.draft).unwrap().unwrap());
    assert!(matches!(&stored.attachments[..], [Attachment::Part { part, .. }] if part == "p2"));
    let refused = h
        .call::<DraftSave>(&update(vec![part("p1")], "k3"))
        .await
        .unwrap_err();
    assert_eq!(
        refused.message,
        "attachments: not an attachment of this draft"
    );
    let on_create = DraftSaveRequest {
        attachments: vec![part("p1")],
        ..forward.clone()
    };
    let on_create = DraftSaveRequest {
        client_token: "k4".into(),
        ..on_create
    };
    assert!(h.call::<DraftSave>(&on_create).await.is_err());
}

fn with_file(path: &std::path::Path, token: &str) -> DraftSaveRequest {
    DraftSaveRequest {
        attachments: vec![DraftAttachment::File {
            path: path.to_string_lossy().into_owned(),
        }],
        ..save_request(None, token)
    }
}

#[tokio::test]
async fn save_refuses_a_missing_file_a_directory_and_a_size_over_the_cap() {
    let h = DaemonHarness::new().await;
    let dir = tempfile::tempdir().unwrap();
    let missing = with_file(&dir.path().join("none.pdf"), "a1");
    let error = h.call::<DraftSave>(&missing).await.unwrap_err();
    assert!(error.message.contains("none.pdf"), "{}", error.message);
    assert!(
        error.message.starts_with("attachments:"),
        "{}",
        error.message
    );
    let directory = with_file(dir.path(), "a2");
    let error = h.call::<DraftSave>(&directory).await.unwrap_err();
    assert!(error.message.contains("regular file"), "{}", error.message);
    let big = dir.path().join("big.bin");
    // Final fix 3: 20 MiB is under 25 raw but over once encoded, as the worker counts it.
    std::fs::write(&big, vec![0u8; 20 * 1024 * 1024]).unwrap();
    let error = h
        .call::<DraftSave>(&with_file(&big, "a3"))
        .await
        .unwrap_err();
    assert!(error.message.contains("25 MiB"), "{}", error.message);
    assert!(error.message.contains("26.6 MiB"), "{}", error.message);
    let small = dir.path().join("Notes.PDF");
    std::fs::write(&small, b"%PDF").unwrap();
    let saved = h.call::<DraftSave>(&with_file(&small, "a4")).await.unwrap();
    let summary = &saved.attachments[0];
    assert_eq!(
        (summary.name.as_str(), summary.mime.as_str(), summary.size),
        ("Notes.PDF", "application/pdf", 4)
    );
    assert_eq!(summary.path.as_deref(), small.to_str());
    std::fs::remove_file(&small).unwrap();
    let vanished = h.send(saved.draft, "a5").await.unwrap_err();
    assert!(
        vanished.message.contains("Notes.PDF"),
        "{}",
        vanished.message
    );
    assert!(
        h.store(|s| s.pending_rows_for_draft(saved.draft).unwrap())
            .is_empty()
    );
}

#[tokio::test]
async fn send_refuses_a_draft_without_recipients_and_one_that_is_gone() {
    let h = DaemonHarness::new().await;
    let empty = DraftSaveRequest {
        to: vec![],
        ..save_request(None, "e1")
    };
    let saved = h.call::<DraftSave>(&empty).await.unwrap();
    let error = h.send(saved.draft, "e2").await.unwrap_err();
    assert!(error.message.starts_with("to:"), "{}", error.message);
    h.call::<DraftDiscard>(&DraftDiscardRequest { draft: saved.draft })
        .await
        .unwrap();
    assert!(h.store(|s| s.draft(saved.draft).unwrap()).is_none());
    h.call::<DraftDiscard>(&DraftDiscardRequest { draft: saved.draft })
        .await
        .expect("discarding again is a no-op");
    let gone = h.send(saved.draft, "e3").await.unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);
    let other = DraftSaveRequest {
        draft: Some(saved.draft),
        ..save_request(None, "e4")
    };
    let gone = h.call::<DraftSave>(&other).await.unwrap_err();
    assert_eq!(gone.code, ErrorCode::NotFound);
}

/// A send the worker holds (inflight) cannot be discarded either.
#[tokio::test]
async fn discard_refuses_a_draft_whose_send_is_inflight() {
    let h = DaemonHarness::new().await;
    let saved = h
        .call::<DraftSave>(&save_request(None, "i1"))
        .await
        .unwrap();
    let row = h.send(saved.draft, "i2").await.unwrap();
    Store::open(&h.paths.db_file())
        .unwrap()
        .conn()
        .execute("UPDATE outbox SET state = 'inflight' WHERE id = ?1", [row])
        .unwrap();
    let error = h
        .call::<DraftDiscard>(&DraftDiscardRequest { draft: saved.draft })
        .await
        .unwrap_err();
    assert!(error.message.contains("undo first"), "{}", error.message);
    assert!(h.store(|s| s.draft(saved.draft).unwrap()).is_some());
    assert_eq!(
        h.store(|s| s.pending_rows_for_draft(saved.draft).unwrap())
            .len(),
        1
    );
}

/// Review Focus 2: a send still pending at a restart stays pending, and an undo must name
/// the token it was queued with.
#[tokio::test]
async fn a_pending_send_survives_a_restart_and_an_old_token_is_too_late() {
    let h = DaemonHarness::new().await;
    let saved = h
        .call::<DraftSave>(&save_request(None, "s1"))
        .await
        .unwrap();
    let row = h.send(saved.draft, "send-1").await.unwrap();
    let h = h.restart().await;
    let pending = h.store(|s| s.pending_rows_for_draft(saved.draft).unwrap());
    assert_eq!(pending.len(), 1, "still pending after the restart");
    let undo = |token: &str| ActionUndoRequest {
        rows: vec![RowToken {
            row,
            client_token: token.into(),
        }],
    };
    let wrong = h.call::<ActionUndo>(&undo("stale")).await.unwrap();
    assert_eq!(wrong.too_late, [row]);
    let right = h.call::<ActionUndo>(&undo("send-1")).await.unwrap();
    assert_eq!(right.undone, [row]);
    let draft = h.store(|s| s.draft(saved.draft).unwrap().unwrap());
    assert_eq!(draft.state, DraftState::Editing);
    assert!(h.mock.with(|m| m.sent.is_empty()), "nothing was sent");
}

/// Discarding a Gmail draft needs no live account: with the account gone from the config (so
/// it has no engine and `action_target` would refuse it), the draft still goes.
#[tokio::test]
async fn a_gmail_draft_is_discarded_while_its_account_is_not_live() {
    let h = DaemonHarness::new().await;
    let saved = h
        .call::<DraftSave>(&save_request(None, "g1"))
        .await
        .unwrap();
    h.client
        .call::<DaemonStop>(&DaemonStopRequest {})
        .await
        .unwrap();
    tokio::time::timeout(BOUND, h.task)
        .await
        .expect("the daemon stops")
        .unwrap()
        .unwrap();
    std::fs::write(h.paths.config_file(), "[general]\nsecrets = \"file\"\n").unwrap();
    let (ctx, _) = daemon_context(&h.paths);
    start_daemon(
        ctx.with_provider_factory(mock_factory(&h.mock)),
        ServeConfig::default(),
    )
    .await;
    let client = Connector::new(h.paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    let live = client
        .call::<SyncStatus>(&SyncStatusRequest { account: None })
        .await
        .unwrap();
    assert!(live.accounts.is_empty(), "no account is live");
    client
        .call::<DraftDiscard>(&DraftDiscardRequest { draft: saved.draft })
        .await
        .expect("a Gmail draft is discarded without a live account");
    let reader = Store::open_reader(&h.paths.db_file()).unwrap();
    assert!(reader.draft(saved.draft).unwrap().is_none());
}
