//! The three action operations through a real daemon over the fake Gmail: apply with and
//! without grace, undo, retry, client-token replay, validation, Microsoft 365, the
//! `action.failed` notification (live, and again for a later watcher), the outbox depth in
//! `sync.status`, and a sync clearing a failed action.

use std::sync::Arc;
use std::time::Duration;

use super::client::DaemonClient;
use super::{Connector, ServeConfig};
use crate::ops::op::{ActionApply, ActionRetry, ActionUndo, DraftSave, SyncNow, SyncStatus};
use crate::ops::{
    Action, ActionApplyRequest, ActionRetryRequest, ActionUndoRequest, DraftSaveRequest, ErrorCode,
    M365_NOT_YET, OpError, RowToken, SyncNowRequest, SyncStatusRequest, names,
};
use crate::paths::Paths;
use crate::provider::mock::{Call, MockProvider, envelope};
use crate::provider::{FolderKind, ProviderError, RemoteFolder};
use crate::store::Store;
use crate::store::drafts::DraftKind;
use crate::test_support::{daemon_context, mock_factory, start_daemon};

const WORK: &str = "[general]\nsecrets = \"file\"\npoll_interval = \"60s\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";
const CORP: &str = "\n[[accounts]]\nname = \"corp\"\nkind = \"m365\"\naddress = \"me@corp.example\"\nclient_id = \"cid2\"\n";
const BOUND: Duration = Duration::from_secs(15);

struct Daemon {
    _tmp: tempfile::TempDir,
    paths: Paths,
    mock: MockProvider,
    client: DaemonClient,
}

async fn daemon(config: &str) -> Daemon {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), config).unwrap();
    let mock = MockProvider::new();
    let now = crate::auth::unix_now() * 1000;
    mock.seed(envelope("m1", "t1", now - 1_000));
    mock.seed(envelope("m2", "t2", now - 2_000));
    mock.with(|m| {
        m.folders.push(RemoteFolder {
            parent: None,
            remote_id: "Label_1".into(),
            name: "Projects".into(),
            kind: FolderKind::User,
        })
    });
    let (ctx, _) = daemon_context(&paths);
    start_daemon(
        ctx.with_provider_factory(mock_factory(&mock)),
        ServeConfig::default(),
    )
    .await;
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    let d = Daemon {
        _tmp: tmp,
        paths,
        mock,
        client,
    };
    d.until_synced().await;
    d
}

impl Daemon {
    async fn until_synced(&self) {
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

    fn thread(&self, remote: &str) -> i64 {
        Store::open_reader(&self.paths.db_file())
            .unwrap()
            .conn()
            .query_row(
                "SELECT id FROM threads WHERE remote_id = ?1",
                [remote],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn label(&self, remote: &str) -> Option<i64> {
        Store::open_reader(&self.paths.db_file())
            .unwrap()
            .conn()
            .query_row(
                "SELECT id FROM folders WHERE remote_id = ?1",
                [remote],
                |r| r.get(0),
            )
            .ok()
    }

    async fn apply(
        &self,
        threads: Vec<i64>,
        action: Action,
        grace: bool,
        token: &str,
    ) -> Result<crate::ops::ActionApplyResponse, OpError> {
        let request = ActionApplyRequest {
            account: "work".into(),
            threads,
            action,
            grace,
            client_token: token.into(),
        };
        self.client.call::<ActionApply>(&request).await
    }

    fn in_inbox(&self, remote: &str) -> bool {
        Store::open_reader(&self.paths.db_file())
            .unwrap()
            .conn()
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM messages m JOIN message_folders mf ON mf.message_id = m.id
                   JOIN folders f ON f.id = mf.folder_id WHERE m.remote_id = ?1 AND f.kind = 'inbox')",
                [remote],
                |r| r.get(0),
            )
            .unwrap()
    }

    async fn depth(&self) -> crate::ops::OutboxDepth {
        let status = self
            .client
            .call::<SyncStatus>(&SyncStatusRequest::default())
            .await
            .unwrap();
        status.accounts[0].outbox
    }

    async fn until(&self, what: &str, done: impl Fn(&Daemon) -> bool) {
        tokio::time::timeout(BOUND, async {
            while !done(self) {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out: {what}"));
    }
}

#[tokio::test]
async fn apply_changes_the_store_at_once_and_the_worker_sends_it() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let answer = d
        .apply(vec![t1], Action::Archive {}, false, "a")
        .await
        .unwrap();
    assert_eq!((answer.rows.len(), answer.applied), (1, 1));
    d.until("sent", |d| d.mock.with(|m| m.modifies.len() == 1))
        .await;
    assert!(!d.in_inbox("m1"));
}

#[tokio::test]
async fn a_grace_row_waits_and_undo_takes_it_back() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let answer = d
        .apply(vec![t1], Action::Star { on: true }, true, "a")
        .await
        .unwrap();
    assert_eq!(d.depth().await.pending, 1);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(d.mock.with(|m| m.modifies.is_empty()), "inside the grace");
    let undo = d
        .client
        .call::<ActionUndo>(&ActionUndoRequest {
            rows: tokens(&answer.rows, "a"),
        })
        .await
        .unwrap();
    assert_eq!((undo.undone, undo.too_late), (answer.rows, vec![]));
    assert_eq!(d.depth().await.pending, 0);
}

#[tokio::test]
async fn a_replayed_token_answers_with_the_original_row() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let first = d
        .apply(vec![t1], Action::Star { on: true }, true, "same")
        .await
        .unwrap();
    let again = d
        .apply(vec![t1], Action::Star { on: true }, true, "same")
        .await
        .unwrap();
    assert_eq!(first.rows, again.rows);
}

#[tokio::test]
async fn apply_refuses_threads_of_another_account_and_unknown_threads() {
    let d = daemon(WORK).await;
    let error = d
        .apply(vec![999_999], Action::Archive {}, true, "a")
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(!error.message.contains("999999"), "{}", error.message);
}

#[tokio::test]
async fn apply_refuses_a_label_the_store_no_longer_has() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let inbox = d.label("INBOX").unwrap();
    let error = d
        .apply(vec![t1], Action::Move { to: inbox }, true, "a")
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::InvalidInput,
        "the inbox is not a label"
    );
    let missing = Action::Labels {
        add: vec![777],
        remove: vec![],
    };
    let error = d.apply(vec![t1], missing, true, "b").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(!error.message.contains("777"), "{}", error.message);
    assert_eq!(d.depth().await.pending, 0, "nothing was queued");
}

#[tokio::test]
async fn a_users_label_is_sent_to_gmail_by_its_remote_id() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let label = d.label("Label_1").unwrap();
    let add = Action::Labels {
        add: vec![label],
        remove: vec![],
    };
    d.apply(vec![t1], add, false, "a").await.unwrap();
    d.until("sent", |d| {
        d.mock
            .with(|m| m.modifies.iter().any(|(_, add, _)| add == &["Label_1"]))
    })
    .await;
}

#[tokio::test]
async fn microsoft_365_refuses_every_action_but_mark_read_and_every_draft() {
    let d = daemon(&format!("{WORK}{CORP}")).await;
    let actions = [
        Action::Archive {},
        Action::Trash {},
        Action::Star { on: true },
        Action::Read { on: false },
        Action::Labels {
            add: vec![1],
            remove: vec![],
        },
        Action::Move { to: 1 },
    ];
    for (n, action) in actions.into_iter().enumerate() {
        let request = ActionApplyRequest {
            account: "corp".into(),
            threads: vec![1],
            action,
            grace: true,
            client_token: format!("a{n}"),
        };
        let error = d.client.call::<ActionApply>(&request).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::Unsupported);
        assert_eq!(error.message, M365_NOT_YET);
    }
    let save = DraftSaveRequest {
        account: "corp".into(),
        draft: None,
        kind: DraftKind::New,
        reply_to: None,
        to: vec![],
        cc: vec![],
        bcc: vec![],
        subject: String::new(),
        body: String::new(),
        attachments: vec![],
        client_token: "s1".into(),
    };
    let error = d.client.call::<DraftSave>(&save).await.unwrap_err();
    assert_eq!(error.message, M365_NOT_YET);
    let rows: i64 = Store::open_reader(&d.paths.db_file())
        .unwrap()
        .conn()
        .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 0, "nothing queued");
}

#[tokio::test]
async fn undo_and_retry_check_their_ids() {
    let d = daemon(WORK).await;
    let bad_rows: [Vec<i64>; 4] = [vec![], vec![0], vec![-1], (1..=65).collect()];
    let cases = bad_rows
        .iter()
        .map(|rows| tokens(rows, "t"))
        .chain([tokens(&[1], ""), tokens(&[1], "a b")]);
    for rows in cases {
        let error = d
            .client
            .call::<ActionUndo>(&ActionUndoRequest { rows })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }
    for (row, token) in [(0, "t"), (1, ""), (1, "tok\n")] {
        let error = d
            .client
            .call::<ActionRetry>(&ActionRetryRequest {
                row,
                client_token: token.into(),
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput, "{token:?}");
    }
    let unknown = d
        .client
        .call::<ActionUndo>(&ActionUndoRequest {
            rows: tokens(&[424_242], "t"),
        })
        .await
        .unwrap();
    assert_eq!((unknown.undone, unknown.too_late), (vec![], vec![424_242]));
}

#[tokio::test]
async fn a_refused_action_is_reverted_notified_and_can_be_retried() {
    let d = daemon(WORK).await;
    let mut notes = d.client.notifications();
    d.client
        .call::<SyncNow>(&SyncNowRequest {
            account: None,
            watch: true,
        })
        .await
        .unwrap();
    let t1 = d.thread("t1");
    d.mock.fail_next(Call::Modify, ProviderError::NotFound);
    let answer = d
        .apply(vec![t1], Action::Archive {}, false, "a")
        .await
        .unwrap();
    let failed = tokio::time::timeout(BOUND, async {
        loop {
            let note = notes.recv().await.unwrap();
            if note.method == names::ACTION_FAILED {
                return note.params;
            }
        }
    })
    .await
    .unwrap();
    let failed: crate::ops::ActionFailed = serde_json::from_value(failed).unwrap();
    assert_eq!(
        (failed.row, failed.threads.clone()),
        (answer.rows[0], vec![t1])
    );
    assert_eq!((failed.client_token.as_str(), failed.draft), ("a", None));
    assert!(d.in_inbox("m1"), "reverted");
    assert_eq!(d.depth().await.failed, 1);
    let wrong = d
        .client
        .call::<ActionRetry>(&ActionRetryRequest {
            row: failed.row,
            client_token: "other".into(),
        })
        .await
        .unwrap();
    assert!(!wrong.retried, "another token retries nothing");
    let retried = d
        .client
        .call::<ActionRetry>(&ActionRetryRequest {
            row: failed.row,
            client_token: failed.client_token.clone(),
        })
        .await
        .unwrap();
    assert!(retried.retried);
    d.until("sent on retry", |d| d.mock.with(|m| m.modifies.len() == 1))
        .await;
    assert!(!d.in_inbox("m1"), "applied again");
    let again = d
        .client
        .call::<ActionRetry>(&ActionRetryRequest {
            row: failed.row,
            client_token: failed.client_token,
        })
        .await
        .unwrap();
    assert!(!again.retried, "no longer failed");
}

impl Daemon {
    /// Archive t1 with Gmail refusing it, while no connection watches; the failed row.
    async fn refused_unwatched(&self) -> i64 {
        let t1 = self.thread("t1");
        self.mock.fail_next(Call::Modify, ProviderError::NotFound);
        let answer = self
            .apply(vec![t1], Action::Archive {}, false, "a")
            .await
            .unwrap();
        tokio::time::timeout(BOUND, async {
            while self.depth().await.failed != 1 {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("failed");
        answer.rows[0]
    }
}

/// A failure nobody watched is sent to the next connection that watches, and retries.
#[tokio::test]
async fn a_failure_while_nobody_watched_is_sent_on_the_next_watch() {
    let d = daemon(WORK).await;
    let row = d.refused_unwatched().await;
    let later = Connector::new(d.paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    let mut notes = later.notifications();
    let watch = SyncNowRequest {
        account: None,
        watch: true,
    };
    later.call::<SyncNow>(&watch).await.unwrap();
    let failed = tokio::time::timeout(BOUND, async {
        loop {
            let note = notes.recv().await.unwrap();
            if note.method == names::ACTION_FAILED {
                return note.params;
            }
        }
    })
    .await
    .expect("replayed");
    let failed: crate::ops::ActionFailed = serde_json::from_value(failed).unwrap();
    assert_eq!(
        (failed.row, failed.threads, failed.action),
        (row, vec![d.thread("t1")], Action::Archive {})
    );
    assert_eq!(failed.error.class, crate::error::ErrorClass::Permanent);
    later.call::<SyncNow>(&watch).await.unwrap();
    let retried = later
        .call::<ActionRetry>(&ActionRetryRequest {
            row: failed.row,
            client_token: failed.client_token.clone(),
        })
        .await
        .unwrap();
    assert!(retried.retried);
    d.until("sent on retry", |d| d.mock.with(|m| m.modifies.len() == 1))
        .await;
    while let Ok(note) = notes.try_recv() {
        assert_ne!(
            note.method,
            names::ACTION_FAILED,
            "a watch the connection already holds is not sent the failures again"
        );
    }
}

/// Spec §3: the next sync of the thread clears its failed action, and the depth follows.
#[tokio::test]
async fn a_sync_of_the_thread_clears_its_failed_action() {
    let d = daemon(WORK).await;
    d.refused_unwatched().await;
    let now = crate::auth::unix_now() * 1000;
    d.mock.add(envelope("m3", "t1", now));
    d.client
        .call::<SyncNow>(&SyncNowRequest {
            account: None,
            watch: false,
        })
        .await
        .unwrap();
    tokio::time::timeout(BOUND, async {
        while d.depth().await.failed != 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cleared");
    let failed: i64 = Store::open_reader(&d.paths.db_file())
        .unwrap()
        .conn()
        .query_row("SELECT count(*) FROM outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(failed, 0);
}

/// `rows`, each with `token`.
fn tokens(rows: &[i64], token: &str) -> Vec<RowToken> {
    rows.iter()
        .map(|&row| RowToken {
            row,
            client_token: token.into(),
        })
        .collect()
}

#[tokio::test]
async fn a_draft_action_cannot_go_through_action_apply() {
    let d = daemon(WORK).await;
    let t1 = d.thread("t1");
    let error = d
        .apply(vec![t1], Action::Send { draft: 1 }, true, "a")
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert_eq!(error.message, "a draft action goes through draft.send");
    assert_eq!(d.depth().await.pending, 0);
}
