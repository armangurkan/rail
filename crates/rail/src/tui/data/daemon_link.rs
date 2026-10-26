//! The mail screen's line to the daemon, on the binary's connector (which starts the daemon
//! when needed and reconnects on the next call after the connection closed). The watch job
//! subscribes to `sync.progress` with `sync.now { watch: true }` (which also wakes every
//! account), reports every status, forwards each change, and reports the loss when the
//! connection ends; `Ctrl-r` runs it again.

use std::time::Duration;

use rail_core::daemon::wire::Notification;
use rail_core::daemon::{Connector, DaemonClient};
use rail_core::error::ErrorClass;
use rail_core::ops::op::{
    ActionApply, ActionRetry, ActionUndo, BodyFetch, DraftDiscard, DraftImport, DraftPush,
    DraftSave, DraftSend, SyncNow, SyncStatus,
};
use rail_core::ops::{
    AccountSyncStatus, ActionApplyRequest, ActionFailed, ActionRetryRequest, ActionUndoRequest,
    BodyFetchRequest, DraftDiscardRequest, DraftId, DraftImportRequest, DraftPushRequest,
    DraftSaveRequest, DraftSendRequest, ErrorCode, OpError, RowToken, SyncNowRequest,
    SyncStatusRequest, names,
};
use tokio::sync::broadcast::{self, error::RecvError};

use super::{DaemonEvent, MailEvent, job, send};
use crate::tui::app::{Effect, TaskSink};

mod snooze;

/// How often the watch checks whether its connection closed (the notification channel lives
/// as long as the client, so it does not end by itself).
const CLOSED_POLL: Duration = Duration::from_secs(1);

/// One `action.apply` the mail screen sends: `seq` ties the calls of one key press together
/// (one per account), so their rows make one undo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyCall {
    pub seq: u64,
    /// The account's name.
    pub account: String,
    pub threads: Vec<i64>,
    pub action: rail_core::ops::Action,
    pub grace: bool,
    /// Made by the screen, one per call, so a resend finds its row and undo or retry can name
    /// the row with the token it was queued under.
    pub client_token: String,
    /// When a snooze ends (Unix seconds): the call is a `snooze.set`, whose archive is `action`.
    pub snooze: Option<i64>,
}

/// One `draft.save` the composer sends; `seq` ties the answer to the save that asked.
#[derive(Clone)]
pub struct SaveCall {
    pub seq: u64,
    pub request: DraftSaveRequest,
}

/// Counts and the kind only: a save carries the message being written.
impl std::fmt::Debug for SaveCall {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let request = &self.request;
        f.debug_struct("SaveCall")
            .field("seq", &self.seq)
            .field("kind", &request.kind)
            .field(
                "recipients",
                &(request.to.len() + request.cc.len() + request.bcc.len()),
            )
            .field("attachments", &request.attachments.len())
            .finish()
    }
}

#[derive(Clone)]
pub struct DaemonLink {
    link: Link,
}

#[derive(Clone)]
enum Link {
    Daemon(Connector),
    #[cfg(test)]
    Fake(std::sync::Arc<FakeDaemon>),
}

impl DaemonLink {
    pub fn new(connector: Connector) -> Self {
        Self {
            link: Link::Daemon(connector),
        }
    }

    #[cfg(test)]
    pub fn fake(fake: std::sync::Arc<FakeDaemon>) -> Self {
        Self {
            link: Link::Fake(fake),
        }
    }

    /// Subscribe and follow the daemon until the connection ends.
    pub fn watch(&self) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let lost = match subscribe(&connector, &sink).await {
                    Ok((client, mut notes)) => follow(&client, &mut notes, &sink).await,
                    Err(error) => error,
                };
                send(&sink, MailEvent::Daemon(DaemonEvent::Lost(lost)));
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move { fake.watch(&sink) }),
        }
    }

    /// Wake every account's sync.
    pub fn refresh(&self) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let request = SyncNowRequest {
                    account: None,
                    watch: false,
                };
                let result = connector.call::<SyncNow>(&request).await.map(|_| ());
                send(&sink, MailEvent::Daemon(DaemonEvent::Refreshed(result)));
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move { fake.refresh(&sink) }),
        }
    }

    /// Fetch the bodies of `ids` (one thread's uncached messages) for `account`.
    pub fn fetch_bodies(&self, thread_id: i64, account: String, ids: Vec<String>) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let request = BodyFetchRequest {
                    account,
                    message_ids: ids,
                };
                let result = connector.call::<BodyFetch>(&request).await;
                send(&sink, MailEvent::Bodies { thread_id, result });
            }),
            #[cfg(test)]
            Link::Fake(fake) => {
                job(move |sink| async move { fake.fetch(&sink, thread_id, account, ids) })
            }
        }
    }

    /// Apply `call`'s action under the call's client token (snooze with it, for a snooze).
    pub fn apply(&self, call: ApplyCall) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let result = if let Some(until) = call.snooze {
                    snooze::set(&connector, &call, until).await
                } else {
                    let request = ActionApplyRequest {
                        account: call.account.clone(),
                        threads: call.threads.clone(),
                        action: call.action.clone(),
                        grace: call.grace,
                        client_token: call.client_token.clone(),
                    };
                    connector.call::<ActionApply>(&request).await
                };
                send(
                    &sink,
                    MailEvent::Applied {
                        call: Box::new(call),
                        result,
                    },
                );
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move { fake.apply(&sink, call) }),
        }
    }

    /// Take back the rows of the action with this `seq`.
    pub fn undo(&self, seq: u64, rows: Vec<RowToken>) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let result = connector
                    .call::<ActionUndo>(&ActionUndoRequest { rows })
                    .await;
                send(&sink, MailEvent::Undone { seq, result });
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move { fake.undo(&sink, seq, rows) }),
        }
    }

    /// Send a failed row again.
    pub fn retry(&self, row: RowToken) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let id = row.row;
                let request = ActionRetryRequest {
                    row: row.row,
                    client_token: row.client_token,
                };
                let result = connector.call::<ActionRetry>(&request).await;
                send(&sink, MailEvent::Retried { row: id, result });
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move { fake.retry(&sink, row) }),
        }
    }

    /// Create or update a draft in the daemon.
    pub fn draft_save(&self, call: SaveCall) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let result = connector.call::<DraftSave>(&call.request).await;
                send(
                    &sink,
                    MailEvent::DraftSaved {
                        seq: call.seq,
                        result,
                    },
                );
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move { fake.draft_save(&sink, call) }),
        }
    }

    /// Queue `draft` to be sent under `client_token` (which `z` and a retry name it by).
    pub fn draft_send(&self, seq: u64, draft: DraftId, client_token: String) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let request = DraftSendRequest {
                    draft,
                    client_token: client_token.clone(),
                };
                let result = connector.call::<DraftSend>(&request).await;
                let event = MailEvent::DraftSent {
                    seq,
                    draft,
                    client_token,
                    result,
                };
                send(&sink, event);
            }),
            #[cfg(test)]
            Link::Fake(fake) => {
                job(move |sink| async move { fake.draft_send(&sink, seq, draft, client_token) })
            }
        }
    }

    /// Save `draft` to Gmail's Drafts under `client_token` (which `z` names the push by).
    pub fn draft_push(&self, seq: u64, draft: DraftId, client_token: String) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let request = DraftPushRequest {
                    draft,
                    client_token: client_token.clone(),
                };
                let result = connector.call::<DraftPush>(&request).await;
                let event = MailEvent::DraftPushed {
                    seq,
                    draft,
                    client_token,
                    result,
                };
                send(&sink, event);
            }),
            #[cfg(test)]
            Link::Fake(fake) => {
                job(move |sink| async move { fake.draft_push(&sink, seq, draft, client_token) })
            }
        }
    }

    /// Make the stored Gmail draft `message` of `account` a local draft, under `client_token`.
    pub fn draft_import(
        &self,
        seq: u64,
        account: String,
        message: i64,
        client_token: String,
    ) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let request = DraftImportRequest {
                    account,
                    message,
                    client_token,
                };
                let result = connector.call::<DraftImport>(&request).await;
                send(
                    &sink,
                    MailEvent::DraftImported {
                        seq,
                        message,
                        result,
                    },
                );
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move {
                fake.draft_import(&sink, seq, account, message, client_token)
            }),
        }
    }

    /// Delete `draft` (the daemon refuses while it is being sent).
    pub fn draft_discard(&self, draft: DraftId) -> Effect {
        match self.link.clone() {
            Link::Daemon(connector) => job(move |sink| async move {
                let result = connector
                    .call::<DraftDiscard>(&DraftDiscardRequest { draft })
                    .await
                    .map(|_| ());
                send(&sink, MailEvent::DraftDiscarded { draft, result });
            }),
            #[cfg(test)]
            Link::Fake(fake) => job(move |sink| async move { fake.draft_discard(&sink, draft) }),
        }
    }
}

/// Connect, subscribe and report every status. The receiver is taken before `sync.now`, so no
/// progress of the woken passes is missed.
async fn subscribe(
    connector: &Connector,
    sink: &TaskSink,
) -> Result<(DaemonClient, broadcast::Receiver<Notification>), OpError> {
    let client = connector.client().await?;
    let notes = client.notifications();
    let request = SyncNowRequest {
        account: None,
        watch: true,
    };
    client.call::<SyncNow>(&request).await?;
    let statuses = client
        .call::<SyncStatus>(&SyncStatusRequest::default())
        .await?
        .accounts;
    send(sink, MailEvent::Daemon(DaemonEvent::Connected(statuses)));
    Ok((client, notes))
}

/// Forward progress until the connection ends; why it ended.
async fn follow(
    client: &DaemonClient,
    notes: &mut broadcast::Receiver<Notification>,
    sink: &TaskSink,
) -> OpError {
    let mut poll = tokio::time::interval(CLOSED_POLL);
    loop {
        tokio::select! {
            note = notes.recv() => match note {
                Ok(note) if note.method == names::SYNC_PROGRESS => progress(note, sink),
                Ok(note) if note.method == names::ACTION_FAILED => action_failed(note, sink),
                Ok(note) if note.method == names::SNOOZE_WOKE => snooze::woke(note, sink),
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    // Missed some: every status again answers them all.
                    match client.call::<SyncStatus>(&SyncStatusRequest::default()).await {
                        Ok(all) => send(sink, MailEvent::Daemon(DaemonEvent::Connected(all.accounts))),
                        Err(error) => return error,
                    }
                }
                Err(RecvError::Closed) => return closed(),
            },
            _ = poll.tick() => {
                if client.is_closed() {
                    return closed();
                }
            }
        }
    }
}

fn progress(note: Notification, sink: &TaskSink) {
    match serde_json::from_value::<AccountSyncStatus>(note.params) {
        Ok(status) => send(
            sink,
            MailEvent::Daemon(DaemonEvent::Progress(Box::new(status))),
        ),
        // The daemon's own JSON; its text is not repeated, the next status corrects it.
        Err(_) => tracing::warn!("a sync.progress notification did not parse"),
    }
}

fn action_failed(note: Notification, sink: &TaskSink) {
    match serde_json::from_value::<ActionFailed>(note.params) {
        Ok(failure) => send(sink, MailEvent::ActionFailed(Box::new(failure))),
        // The daemon's own JSON, and `detail` may hold Gmail's text: nothing is repeated.
        Err(_) => tracing::warn!("an action.failed notification did not parse"),
    }
}

fn closed() -> OpError {
    OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Transient,
        "the connection to the daemon closed",
    )
}

/// A scripted daemon for screen tests: records each call and answers at once.
#[cfg(test)]
#[derive(Default)]
pub struct FakeDaemon {
    pub calls: std::sync::Mutex<Vec<FakeCall>>,
    pub statuses: std::sync::Mutex<Vec<AccountSyncStatus>>,
    pub unreachable: std::sync::atomic::AtomicBool,
    /// The answer to the next `body.fetch`; every id `fetched` when unset.
    pub answer: std::sync::Mutex<Option<Result<rail_core::ops::BodyFetchResponse, OpError>>>,
    /// Runs with the ids of each `body.fetch` before the fake answers (a test writes the
    /// bodies to the store here, as the daemon would).
    #[allow(clippy::type_complexity, reason = "a test hook, named once")]
    pub on_fetch: std::sync::Mutex<Option<Box<dyn FnMut(&[String]) + Send>>>,
    /// Runs with each `action.apply` before the fake answers (a test applies it to the store
    /// here, as the daemon would); its answer is the fake's. Rows `[n]` counting up when unset.
    #[allow(clippy::type_complexity, reason = "a test hook, named once")]
    pub on_apply: std::sync::Mutex<
        Option<
            Box<
                dyn FnMut(&ApplyCall) -> Result<rail_core::ops::ActionApplyResponse, OpError>
                    + Send,
            >,
        >,
    >,
    /// The answer to the next `action.undo`; every row undone when unset.
    pub undo_answer: std::sync::Mutex<Option<Result<rail_core::ops::ActionUndoResponse, OpError>>>,
    /// The answer to the next `action.retry`; retried when unset.
    pub retry_answer:
        std::sync::Mutex<Option<Result<rail_core::ops::ActionRetryResponse, OpError>>>,
    pub next_row: std::sync::atomic::AtomicI64,
    /// Runs with each `draft.save` before the fake answers (a test checks or stores the draft
    /// here); its answer is the fake's. A fresh draft id and `<fake@example.com>` when unset.
    #[allow(clippy::type_complexity, reason = "a test hook, named once")]
    pub on_save: std::sync::Mutex<
        Option<
            Box<dyn FnMut(&SaveCall) -> Result<rail_core::ops::DraftSaveResponse, OpError> + Send>,
        >,
    >,
    /// The answer to the next `draft.send`; a fresh row when unset.
    pub send_answer: std::sync::Mutex<Option<Result<rail_core::ops::DraftSendResponse, OpError>>>,
    /// The answer to the next `draft.push`; a fresh row when unset.
    pub push_answer: std::sync::Mutex<Option<Result<rail_core::ops::DraftPushResponse, OpError>>>,
    /// The answer to the next `draft.import`; a fresh draft when unset.
    pub import_answer:
        std::sync::Mutex<Option<Result<rail_core::ops::DraftImportResponse, OpError>>>,
    /// The answer to the next `draft.discard`; discarded when unset.
    pub discard_answer: std::sync::Mutex<Option<Result<(), OpError>>>,
    /// The answer to the next `snooze.set`; a fresh row and every thread when unset.
    pub snooze_answer: std::sync::Mutex<Option<Result<rail_core::ops::SnoozeSetResponse, OpError>>>,
    pub next_draft: std::sync::atomic::AtomicI64,
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    Watch,
    Refresh,
    Fetch {
        account: String,
        ids: Vec<String>,
    },
    Apply {
        account: String,
        threads: Vec<i64>,
        action: rail_core::ops::Action,
        grace: bool,
    },
    Undo {
        rows: Vec<RowToken>,
    },
    Retry {
        row: RowToken,
    },
    DraftSave {
        kind: rail_core::store::drafts::DraftKind,
        to: Vec<String>,
        subject: String,
        attachments: usize,
    },
    DraftSend {
        draft: DraftId,
        client_token: String,
    },
    DraftPush {
        draft: DraftId,
        client_token: String,
    },
    DraftImport {
        account: String,
        message: i64,
        client_token: String,
    },
    DraftDiscard {
        draft: DraftId,
    },
    SnoozeSet {
        account: String,
        threads: Vec<i64>,
        until: i64,
        grace: bool,
    },
    SnoozeClear {
        account: String,
        threads: Vec<i64>,
    },
}

#[cfg(test)]
impl FakeDaemon {
    fn record(&self, call: FakeCall) {
        self.calls.lock().unwrap().push(call);
    }

    fn down(&self) -> bool {
        self.unreachable.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn watch(&self, sink: &TaskSink) {
        self.record(FakeCall::Watch);
        let event = if self.down() {
            DaemonEvent::Lost(closed())
        } else {
            DaemonEvent::Connected(self.statuses.lock().unwrap().clone())
        };
        send(sink, MailEvent::Daemon(event));
    }

    fn refresh(&self, sink: &TaskSink) {
        self.record(FakeCall::Refresh);
        let result = if self.down() { Err(closed()) } else { Ok(()) };
        send(sink, MailEvent::Daemon(DaemonEvent::Refreshed(result)));
    }

    fn fetch(&self, sink: &TaskSink, thread_id: i64, account: String, ids: Vec<String>) {
        use rail_core::ops::{BodyFetchResponse, BodyFetchResult, BodyOutcome};
        if let Some(hook) = self.on_fetch.lock().unwrap().as_mut() {
            hook(&ids);
        }
        self.record(FakeCall::Fetch {
            account,
            ids: ids.clone(),
        });
        let result = self.answer.lock().unwrap().take().unwrap_or_else(|| {
            Ok(BodyFetchResponse {
                results: ids
                    .into_iter()
                    .map(|message_id| BodyFetchResult {
                        message_id,
                        outcome: BodyOutcome::Fetched,
                    })
                    .collect(),
            })
        });
        send(sink, MailEvent::Bodies { thread_id, result });
    }

    fn apply(&self, sink: &TaskSink, call: ApplyCall) {
        use std::sync::atomic::Ordering;
        if let Some(until) = call.snooze {
            return self.snooze(sink, call, until);
        }
        self.record(FakeCall::Apply {
            account: call.account.clone(),
            threads: call.threads.clone(),
            action: call.action.clone(),
            grace: call.grace,
        });
        let hooked = self
            .on_apply
            .lock()
            .unwrap()
            .as_mut()
            .map(|hook| hook(&call));
        let result = hooked.unwrap_or_else(|| {
            let row = self.next_row.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(rail_core::ops::ActionApplyResponse {
                rows: vec![row],
                applied: call.threads.len(),
            })
        });
        let result = if self.down() { Err(closed()) } else { result };
        send(
            sink,
            MailEvent::Applied {
                call: Box::new(call),
                result,
            },
        );
    }

    fn undo(&self, sink: &TaskSink, seq: u64, rows: Vec<RowToken>) {
        self.record(FakeCall::Undo { rows: rows.clone() });
        let undone = rows.iter().map(|r| r.row).collect();
        let result = self.undo_answer.lock().unwrap().take().unwrap_or(Ok(
            rail_core::ops::ActionUndoResponse {
                undone,
                too_late: vec![],
            },
        ));
        send(sink, MailEvent::Undone { seq, result });
    }

    fn retry(&self, sink: &TaskSink, row: RowToken) {
        self.record(FakeCall::Retry { row: row.clone() });
        let result = self
            .retry_answer
            .lock()
            .unwrap()
            .take()
            .unwrap_or(Ok(rail_core::ops::ActionRetryResponse { retried: true }));
        send(
            sink,
            MailEvent::Retried {
                row: row.row,
                result,
            },
        );
    }

    fn draft_save(&self, sink: &TaskSink, call: SaveCall) {
        use std::sync::atomic::Ordering;
        let request = &call.request;
        self.record(FakeCall::DraftSave {
            kind: request.kind,
            to: request.to.clone(),
            subject: request.subject.clone(),
            attachments: request.attachments.len(),
        });
        let hooked = self
            .on_save
            .lock()
            .unwrap()
            .as_mut()
            .map(|hook| hook(&call));
        let result = hooked.unwrap_or_else(|| {
            Ok(rail_core::ops::DraftSaveResponse {
                draft: request
                    .draft
                    .unwrap_or_else(|| self.next_draft.fetch_add(1, Ordering::SeqCst) + 1),
                message_id: "<fake@example.com>".to_string(),
                attachments: Vec::new(),
            })
        });
        let result = if self.down() { Err(closed()) } else { result };
        send(
            sink,
            MailEvent::DraftSaved {
                seq: call.seq,
                result,
            },
        );
    }

    fn draft_send(&self, sink: &TaskSink, seq: u64, draft: DraftId, client_token: String) {
        use std::sync::atomic::Ordering;
        self.record(FakeCall::DraftSend {
            draft,
            client_token: client_token.clone(),
        });
        let result = self.send_answer.lock().unwrap().take().unwrap_or_else(|| {
            Ok(rail_core::ops::DraftSendResponse {
                row: self.next_row.fetch_add(1, Ordering::SeqCst) + 1,
            })
        });
        let result = if self.down() { Err(closed()) } else { result };
        let event = MailEvent::DraftSent {
            seq,
            draft,
            client_token,
            result,
        };
        send(sink, event);
    }

    fn draft_push(&self, sink: &TaskSink, seq: u64, draft: DraftId, client_token: String) {
        use std::sync::atomic::Ordering;
        self.record(FakeCall::DraftPush {
            draft,
            client_token: client_token.clone(),
        });
        let result = self.push_answer.lock().unwrap().take().unwrap_or_else(|| {
            Ok(rail_core::ops::DraftPushResponse {
                row: self.next_row.fetch_add(1, Ordering::SeqCst) + 1,
            })
        });
        let result = if self.down() { Err(closed()) } else { result };
        let event = MailEvent::DraftPushed {
            seq,
            draft,
            client_token,
            result,
        };
        send(sink, event);
    }

    fn draft_import(
        &self,
        sink: &TaskSink,
        seq: u64,
        account: String,
        message: i64,
        client_token: String,
    ) {
        use std::sync::atomic::Ordering;
        self.record(FakeCall::DraftImport {
            account,
            message,
            client_token,
        });
        let result = self
            .import_answer
            .lock()
            .unwrap()
            .take()
            .unwrap_or_else(|| {
                Ok(rail_core::ops::DraftImportResponse {
                    draft: self.next_draft.fetch_add(1, Ordering::SeqCst) + 1,
                })
            });
        let result = if self.down() { Err(closed()) } else { result };
        send(
            sink,
            MailEvent::DraftImported {
                seq,
                message,
                result,
            },
        );
    }

    fn draft_discard(&self, sink: &TaskSink, draft: DraftId) {
        self.record(FakeCall::DraftDiscard { draft });
        let result = self.discard_answer.lock().unwrap().take().unwrap_or(Ok(()));
        let result = if self.down() { Err(closed()) } else { result };
        send(sink, MailEvent::DraftDiscarded { draft, result });
    }
}
