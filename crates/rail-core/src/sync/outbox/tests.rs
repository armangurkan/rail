//! The worker over `MockProvider` and a real writer: FIFO, the Gmail calls each action makes,
//! backoff and the cap, permanent failure (revert and notice), pause and resume, the restart
//! rule, the undo race, rows whose threads are gone, a label gone before sending, shutdown.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{Notify, watch};

use super::{Worker, WorkerParts};
use crate::config::AccountKind;
use crate::ops::{AccountSyncStatus, Action, ActionFailed, RowToken};
use crate::provider::mock::{Call, MockProvider, envelope};
use crate::provider::{FolderKind, ProviderError, RemoteFolder, ThreadTarget};
use crate::store::Store;
use crate::store::outbox::{NewAction, OutboxOutcome, OutboxWrite};
use crate::sync::batch::Batch;
use crate::sync::status::StatusBoard;
use crate::sync::writer::{WriterHandle, spawn_writer};

pub(super) const ADDRESS: &str = "me@example.com";
pub(super) const WAIT: Duration = Duration::from_secs(10);

pub(super) struct Rig {
    _tmp: tempfile::TempDir,
    pub(super) db: std::path::PathBuf,
    pub(super) writer: WriterHandle,
    pub(super) mock: MockProvider,
    pub(super) board: StatusBoard,
    pub(super) signal: Arc<Notify>,
    pub(super) stop: watch::Sender<bool>,
    pub(super) failures: Arc<Mutex<Vec<ActionFailed>>>,
    pub(super) task: Option<tokio::task::JoinHandle<()>>,
}

fn folders() -> Vec<RemoteFolder> {
    vec![
        RemoteFolder {
            parent: None,
            remote_id: "INBOX".into(),
            name: "Inbox".into(),
            kind: FolderKind::Inbox,
        },
        RemoteFolder {
            parent: None,
            remote_id: "TRASH".into(),
            name: "Trash".into(),
            kind: FolderKind::Trash,
        },
        RemoteFolder {
            parent: None,
            remote_id: "Label_1".into(),
            name: "Projects".into(),
            kind: FolderKind::User,
        },
    ]
}

pub(super) async fn rig() -> Rig {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("rail.db");
    Store::open(&db)
        .unwrap()
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', ?1)",
            [ADDRESS],
        )
        .unwrap();
    let (writer, _task) = spawn_writer(&db).unwrap();
    let batch = Batch {
        folders: Some(folders()),
        upserts: vec![envelope("m1", "t1", 1_000), envelope("m2", "t2", 2_000)],
        ..Batch::new(1)
    };
    writer.write(batch).await.unwrap();
    let board = StatusBoard::default();
    board.insert(AccountSyncStatus::new("work", ADDRESS, AccountKind::Gmail));
    let failures = Arc::new(Mutex::new(Vec::new()));
    let seen = failures.clone();
    board.subscribe_failures(Box::new(move |f| {
        seen.lock().unwrap().push(f.clone());
        true
    }));
    let (stop, _) = watch::channel(false);
    Rig {
        _tmp: tmp,
        db,
        writer,
        mock: MockProvider::new(),
        board,
        signal: Arc::new(Notify::new()),
        stop,
        failures,
        task: None,
    }
}

impl Rig {
    pub(super) fn worker(&self) -> Worker {
        self.worker_with(None, Arc::new(Notify::new()))
    }

    pub(super) fn worker_with(&self, from_name: Option<String>, sync_wake: Arc<Notify>) -> Worker {
        self.worker_on(Arc::new(self.mock.clone()), from_name, sync_wake)
    }

    /// A worker over another provider (a fake Gmail).
    pub(super) fn worker_on(
        &self,
        provider: Arc<dyn crate::provider::Provider>,
        from_name: Option<String>,
        sync_wake: Arc<Notify>,
    ) -> Worker {
        Worker::new(WorkerParts {
            name: "work".into(),
            address: ADDRESS.into(),
            account_id: 1,
            provider,
            writer: self.writer.clone(),
            reader: Store::open_reader(&self.db).unwrap(),
            board: self.board.clone(),
            signal: self.signal.clone(),
            clock: Arc::new(crate::auth::unix_now),
            from_name,
            sync_wake,
            draft_files_dir: self.draft_files_dir(),
        })
    }

    /// Where this rig's imported drafts keep their files.
    pub(super) fn draft_files_dir(&self) -> std::path::PathBuf {
        self._tmp.path().join("draft-files")
    }

    pub(super) fn start(&mut self) {
        self.task = Some(tokio::spawn(self.worker().run(self.stop.subscribe())));
    }

    async fn enqueue(&self, action: Action, threads: Vec<i64>, token: &str) -> i64 {
        let new = NewAction {
            account_id: 1,
            action,
            threads,
            not_before: 0,
            client_token: token.into(),
        };
        let OutboxOutcome::Enqueued { rows, .. } =
            self.writer.outbox(OutboxWrite::Enqueue(new)).await.unwrap()
        else {
            panic!("enqueued");
        };
        self.signal.notify_one();
        rows[0]
    }

    pub(super) fn store(&self) -> Store {
        Store::open_reader(&self.db).unwrap()
    }

    pub(super) async fn until(&self, what: &str, done: impl Fn(&Rig) -> bool) {
        tokio::time::timeout(WAIT, async {
            while !done(self) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
    }

    fn depth(&self) -> (u64, u64, u64) {
        let counts = self.store().outbox_depth(1).unwrap();
        (counts.pending, counts.failed, counts.sending)
    }
}

fn t(store: &Store, remote: &str) -> i64 {
    store
        .conn()
        .query_row(
            "SELECT id FROM threads WHERE remote_id = ?1",
            [remote],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn each_action_makes_the_gmail_call_the_spec_maps_it_to_in_order() {
    let mut rig = rig().await;
    let (t1, t2) = {
        let s = rig.store();
        (t(&s, "t1"), t(&s, "t2"))
    };
    let label: i64 = rig
        .store()
        .conn()
        .query_row(
            "SELECT id FROM folders WHERE remote_id = 'Label_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    rig.enqueue(Action::Archive {}, vec![t1], "a").await;
    rig.enqueue(Action::Star { on: true }, vec![t2], "b").await;
    rig.enqueue(Action::Read { on: true }, vec![t1, t2], "c")
        .await;
    rig.enqueue(Action::Move { to: label }, vec![t2], "d").await;
    rig.enqueue(Action::Trash {}, vec![t1], "e").await;
    rig.start();
    rig.until("every row sent", |r| r.depth() == (0, 0, 0))
        .await;
    // The worker publishes the depth after the row's delete commits, so the board lags the store.
    rig.until("the board shows nothing pending", |r| {
        r.board.get(ADDRESS).is_some_and(|s| s.outbox.pending == 0)
    })
    .await;
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    let modifies = rig.mock.with(|m| m.modifies.clone());
    assert_eq!(
        modifies,
        [
            (s(&["t1"]), s(&[]), s(&["INBOX"])),
            (s(&["t2"]), s(&["STARRED"]), s(&[])),
            (s(&["t1"]), s(&[]), s(&["UNREAD"])),
            (s(&["t2"]), s(&[]), s(&["UNREAD"])),
            (s(&["t2"]), s(&["Label_1"]), s(&["INBOX"])),
        ]
    );
    assert_eq!(rig.mock.with(|m| m.trashed.clone()), ["t1"]);
    assert_eq!(rig.board.get(ADDRESS).unwrap().outbox.pending, 0);
}

#[tokio::test]
async fn a_transient_failure_backs_off_and_keeps_the_change() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    rig.mock
        .fail_next(Call::Modify, ProviderError::transient("Gmail is busy"));
    let id = rig.enqueue(Action::Archive {}, vec![t1], "a").await;
    rig.start();
    rig.until("the failure recorded", |r| {
        r.store()
            .conn()
            .query_row("SELECT attempts FROM outbox WHERE id = ?1", [id], |x| {
                x.get::<_, i64>(0)
            })
            .unwrap_or(0)
            == 1
    })
    .await;
    let not_before: i64 = rig
        .store()
        .conn()
        .query_row("SELECT not_before FROM outbox WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(
        not_before >= crate::auth::unix_now() + 55,
        "about a minute away"
    );
    assert!(rig.failures.lock().unwrap().is_empty());
    assert_eq!(rig.board.get(ADDRESS).unwrap().outbox.pending, 1);
}

#[tokio::test]
async fn a_permanent_failure_reverts_and_notifies() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    rig.mock.fail_next(Call::Modify, ProviderError::NotFound);
    let id = rig.enqueue(Action::Archive {}, vec![t1], "a").await;
    rig.start();
    rig.until("the notice", |r| !r.failures.lock().unwrap().is_empty())
        .await;
    let failure = rig.failures.lock().unwrap()[0].clone();
    assert_eq!(
        (
            failure.account.as_str(),
            failure.row,
            failure.threads.clone()
        ),
        ("work", id, vec![t1])
    );
    assert_eq!(failure.action, Action::Archive {});
    let inbox: i64 = rig.store().conn().query_row(
        "SELECT count(*) FROM message_folders mf JOIN folders f ON f.id = mf.folder_id WHERE f.kind = 'inbox'", [], |r| r.get(0),
    ).unwrap();
    assert_eq!(inbox, 2, "m1 is back in the inbox");
    assert_eq!(rig.depth(), (0, 1, 0));
    assert_eq!(rig.board.get(ADDRESS).unwrap().outbox.failed, 1);
}

#[tokio::test]
async fn needs_auth_pauses_the_worker_until_it_is_woken() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    rig.mock
        .fail_next(Call::Modify, ProviderError::NeedsAuth { detail: None });
    rig.enqueue(Action::Archive {}, vec![t1], "a").await;
    rig.start();
    rig.until("paused", |r| r.board.get(ADDRESS).unwrap().needs_auth)
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(rig.depth(), (1, 0, 0), "kept, not failed");
    let attempts: i64 = rig
        .store()
        .conn()
        .query_row("SELECT attempts FROM outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(attempts, 0);
    rig.board.update(ADDRESS, |s| s.needs_auth = false);
    rig.signal.notify_one();
    rig.until("sent after the login", |r| r.depth() == (0, 0, 0))
        .await;
}

#[tokio::test]
async fn an_inflight_row_at_start_is_sent_again_once() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    rig.enqueue(Action::Archive {}, vec![t1], "a").await;
    // A daemon that died mid-call left the row claimed.
    rig.writer
        .outbox(OutboxWrite::Claim { account_id: 1 })
        .await
        .unwrap();
    rig.start();
    rig.until("sent", |r| r.depth() == (0, 0, 0)).await;
    let modifies = rig.mock.with(|m| m.modifies.clone());
    assert_eq!(modifies.len(), 1);
    assert_eq!(modifies[0].0, ["t1"]);
}

#[tokio::test]
async fn an_undo_after_the_claim_is_too_late() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    rig.mock
        .with(|m| m.action_delay = Duration::from_millis(300));
    let id = rig.enqueue(Action::Archive {}, vec![t1], "a").await;
    rig.start();
    rig.until("claimed", |r| r.mock.calls().contains(&Call::Modify))
        .await;
    let undo = rig
        .writer
        .outbox(OutboxWrite::Undo {
            rows: vec![RowToken {
                row: id,
                client_token: "a".into(),
            }],
        })
        .await
        .unwrap();
    assert_eq!(
        undo,
        OutboxOutcome::Undone {
            undone: vec![],
            too_late: vec![id]
        }
    );
    rig.until("sent", |r| r.depth() == (0, 0, 0)).await;
}

#[tokio::test]
async fn a_row_whose_threads_are_gone_completes_without_a_call() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    rig.enqueue(Action::Star { on: true }, vec![t1], "a").await;
    rig.writer
        .write(Batch {
            deletes: vec!["m1".into()],
            ..Batch::new(1)
        })
        .await
        .unwrap();
    // A new thread that may reuse t1's rowid arrives before the worker runs.
    rig.writer
        .write(Batch {
            upserts: vec![envelope("m5", "t5", 5_000)],
            ..Batch::new(1)
        })
        .await
        .unwrap();
    rig.start();
    rig.until("done", |r| r.depth() == (0, 0, 0)).await;
    assert!(rig.mock.with(|m| m.modifies.is_empty()), "nothing sent");
}

#[tokio::test]
async fn a_label_gone_before_sending_fails_and_reverts() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    let label: i64 = rig
        .store()
        .conn()
        .query_row(
            "SELECT id FROM folders WHERE remote_id = 'Label_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    rig.enqueue(
        Action::Labels {
            add: vec![label],
            remove: vec![],
        },
        vec![t1],
        "a",
    )
    .await;
    // The labels step of a pass: Label_1 was deleted in Gmail.
    let without: Vec<RemoteFolder> = folders()
        .into_iter()
        .filter(|f| f.remote_id != "Label_1")
        .collect();
    rig.writer
        .write(Batch {
            folders: Some(without),
            ..Batch::new(1)
        })
        .await
        .unwrap();
    rig.start();
    rig.until("the notice", |r| !r.failures.lock().unwrap().is_empty())
        .await;
    let error = rig.failures.lock().unwrap()[0].error.message.clone();
    assert_eq!(error, "a label of this action no longer exists in Gmail");
    assert!(rig.mock.with(|m| m.modifies.is_empty()));
    assert_eq!(rig.depth(), (0, 1, 0));
}

#[tokio::test]
async fn stop_ends_the_worker_inside_a_slow_call_and_the_row_stays_claimed() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    rig.mock.with(|m| m.action_delay = Duration::from_secs(30));
    rig.enqueue(Action::Archive {}, vec![t1], "a").await;
    rig.start();
    rig.until("calling", |r| r.mock.calls().contains(&Call::Modify))
        .await;
    rig.stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(2), rig.task.take().unwrap())
        .await
        .expect("stopped in time")
        .unwrap();
    let state: String = rig
        .store()
        .conn()
        .query_row("SELECT state FROM outbox", [], |r| r.get(0))
        .unwrap();
    assert_eq!(state, "inflight", "the restart rule sends it again");
}

#[tokio::test]
async fn a_failure_part_way_through_the_threads_fails_the_row_after_one_call_each() {
    let mut rig = rig().await;
    let (t1, t2) = {
        let s = rig.store();
        (t(&s, "t1"), t(&s, "t2"))
    };
    rig.mock
        .fail_after(Call::Modify, 1, ProviderError::NotFound);
    rig.enqueue(Action::Star { on: true }, vec![t1, t2], "a")
        .await;
    rig.start();
    rig.until("the notice", |r| !r.failures.lock().unwrap().is_empty())
        .await;
    let modifies = rig.mock.with(|m| m.modifies.clone());
    assert_eq!(modifies.len(), 1, "t1 was sent, t2 refused");
    assert_eq!(modifies[0].0, ["t1"]);
    assert_eq!(rig.depth(), (0, 1, 0), "the whole row failed");
}

#[tokio::test]
async fn a_blocked_row_does_not_make_the_worker_poll() {
    let rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    let later = crate::auth::unix_now() + 3_600;
    let new = |not_before, token: &str| NewAction {
        account_id: 1,
        action: Action::Star { on: true },
        threads: vec![t1],
        not_before,
        client_token: token.into(),
    };
    rig.writer
        .outbox(OutboxWrite::Enqueue(new(later, "a")))
        .await
        .unwrap();
    rig.writer
        .outbox(OutboxWrite::Enqueue(new(0, "b")))
        .await
        .unwrap();
    // b is due but held behind a: nothing to claim, and no reason to wake before a is due.
    let worker = rig.worker();
    assert!(matches!(worker.step().await, super::Next::Idle(wait) if wait == super::MAX_IDLE));
}

#[tokio::test]
async fn a_paused_worker_notices_the_flag_cleared_without_a_wake() {
    let rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    rig.mock
        .fail_next(Call::Modify, ProviderError::NeedsAuth { detail: None });
    rig.enqueue(Action::Archive {}, vec![t1], "a").await;
    let mut worker = rig.worker();
    worker.paused_poll = super::MIN_IDLE;
    let mut rig = rig;
    rig.task = Some(tokio::spawn(worker.run(rig.stop.subscribe())));
    rig.until("paused", |r| r.board.get(ADDRESS).unwrap().needs_auth)
        .await;
    // The engine clears the flag (a good labels step) and does not notify the worker.
    rig.board.update(ADDRESS, |s| s.needs_auth = false);
    rig.until("sent without a wake", |r| r.depth() == (0, 0, 0))
        .await;
    assert_eq!(rig.mock.with(|m| m.modifies.len()), 1);
}

#[tokio::test]
async fn a_row_that_gave_up_is_reported_as_final() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    let id = rig.enqueue(Action::Archive {}, vec![t1], "a").await;
    Store::open(&rig.db)
        .unwrap()
        .conn()
        .execute("UPDATE outbox SET attempts = 19 WHERE id = ?1", [id])
        .unwrap();
    rig.mock
        .fail_next(Call::Modify, ProviderError::transient("Gmail is busy"));
    rig.start();
    rig.until("the notice", |r| !r.failures.lock().unwrap().is_empty())
        .await;
    let error = rig.failures.lock().unwrap()[0].error.clone();
    assert_eq!(error.message, "gave up after 20 attempts");
    assert_eq!(error.class, crate::error::ErrorClass::Permanent);
    assert_eq!(error.code, crate::ops::ErrorCode::Provider);
}

#[tokio::test]
async fn a_read_row_carries_each_threads_stored_message_ids() {
    let mut rig = rig().await;
    let t1 = t(&rig.store(), "t1");
    rig.enqueue(Action::Read { on: true }, vec![t1], "a").await;
    rig.start();
    rig.until("the row sent", |r| r.depth() == (0, 0, 0)).await;
    let targets = rig.mock.with(|m| m.modify_targets.clone());
    let expected = ThreadTarget {
        thread_id: "t1".into(),
        message_ids: vec!["m1".into()],
    };
    assert_eq!(targets, [vec![expected]]);
}
