//! The mail screen's line to the daemon, on the binary's connector (which starts the daemon
//! when needed and reconnects on the next call after the connection closed). The watch job
//! subscribes to `sync.progress` with `sync.now { watch: true }` (which also wakes every
//! account), reports every status, forwards each change, and reports the loss when the
//! connection ends; `Ctrl-r` runs it again.

use std::time::Duration;

use rail_core::daemon::wire::Notification;
use rail_core::daemon::{Connector, DaemonClient};
use rail_core::error::ErrorClass;
use rail_core::ops::op::{BodyFetch, SyncNow, SyncStatus};
use rail_core::ops::{
    AccountSyncStatus, BodyFetchRequest, ErrorCode, OpError, SyncNowRequest, SyncStatusRequest,
    names,
};
use tokio::sync::broadcast::{self, error::RecvError};

use super::{DaemonEvent, MailEvent, job, send};
use crate::tui::app::{Effect, TaskSink};

/// How often the watch checks whether its connection closed (the notification channel lives
/// as long as the client, so it does not end by itself).
const CLOSED_POLL: Duration = Duration::from_secs(1);

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
}

#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FakeCall {
    Watch,
    Refresh,
    Fetch { account: String, ids: Vec<String> },
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
}
