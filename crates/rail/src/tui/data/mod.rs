//! The mail screen's two data paths, both answering through the event loop as `MailEvent`s:
//! the store, read on blocking tasks (`StoreReader`), and the daemon (`DaemonLink`). Debounce
//! timers are jobs too, so nothing runs while nothing happens.

mod daemon_link;
mod store_reader;
#[cfg(test)]
mod tests;

use std::fmt;
use std::panic::AssertUnwindSafe;
use std::time::Duration;

use futures_util::FutureExt;

use rail_core::ops::{
    AccountSyncStatus, ActionApplyResponse, ActionFailed, ActionRetryResponse, ActionUndoResponse,
    BodyFetchResponse, OpError, OutboxId,
};
use rail_core::store::view::{HtmlBody, LabelChoices, ThreadDetail};

pub use daemon_link::{ApplyCall, DaemonLink};
#[cfg(test)]
pub use daemon_link::{FakeCall, FakeDaemon};
pub use store_reader::{
    ListOutcome, ListRequest, ReadError, ScopeChoice, SidebarOutcome, StoreReader,
};

use crate::tui::app::{Effect, TaskResult, TaskSink};

/// How long the store must stay quiet after a change before the list is read again.
pub const REQUERY_DEBOUNCE: Duration = Duration::from_millis(100);
/// How long typing in the filter line must pause before the list is filtered.
pub const FILTER_DEBOUNCE: Duration = Duration::from_millis(150);

/// What a debounce timer was for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Timer {
    Requery,
    Filter,
}

/// What the daemon link reports.
pub enum DaemonEvent {
    /// Subscribed; every account's status.
    Connected(Vec<AccountSyncStatus>),
    /// One account's status changed.
    Progress(Box<AccountSyncStatus>),
    /// The connection failed or ended.
    Lost(OpError),
    /// `sync.now` (Ctrl-r) answered.
    Refreshed(Result<(), OpError>),
}

/// One answer to the mail screen.
pub enum MailEvent {
    Listed {
        seq: u64,
        result: Result<ListOutcome, ReadError>,
    },
    Sidebar {
        seq: u64,
        result: Result<SidebarOutcome, ReadError>,
    },
    Thread {
        seq: u64,
        thread_id: i64,
        result: Result<Option<ThreadDetail>, ReadError>,
    },
    Html {
        message_id: i64,
        result: Result<Option<HtmlBody>, ReadError>,
    },
    Bodies {
        thread_id: i64,
        result: Result<BodyFetchResponse, OpError>,
    },
    /// `action.apply` answered for `call`.
    Applied {
        call: ApplyCall,
        result: Result<ActionApplyResponse, OpError>,
    },
    Undone(Result<ActionUndoResponse, OpError>),
    Retried {
        row: OutboxId,
        result: Result<ActionRetryResponse, OpError>,
    },
    /// The daemon gave up on an action and reverted it.
    ActionFailed(Box<ActionFailed>),
    Labels {
        seq: u64,
        result: Result<LabelChoices, ReadError>,
    },
    /// The refresh with this sequence number gave its last answer, or its job panicked.
    RefreshEnded {
        seq: u64,
    },
    Daemon(DaemonEvent),
    Timer {
        token: u64,
        kind: Timer,
    },
}

/// Names and numbers only: a `MailEvent` carries mail, which must never reach a log.
impl fmt::Debug for MailEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Listed { seq, result } => f
                .debug_struct("Listed")
                .field("seq", seq)
                .field("ok", &result.is_ok())
                .finish(),
            Self::Sidebar { seq, result } => f
                .debug_struct("Sidebar")
                .field("seq", seq)
                .field("ok", &result.is_ok())
                .finish(),
            Self::Thread {
                seq,
                thread_id,
                result,
            } => f
                .debug_struct("Thread")
                .field("seq", seq)
                .field("thread_id", thread_id)
                .field("ok", &result.is_ok())
                .finish(),
            Self::Html { message_id, .. } => f
                .debug_struct("Html")
                .field("message_id", message_id)
                .finish(),
            Self::Bodies { thread_id, result } => f
                .debug_struct("Bodies")
                .field("thread_id", thread_id)
                .field("ok", &result.is_ok())
                .finish(),
            Self::Applied { call, result } => f
                .debug_struct("Applied")
                .field("seq", &call.seq)
                .field("threads", &call.threads.len())
                .field("ok", &result.is_ok())
                .finish(),
            Self::Undone(result) => f
                .debug_struct("Undone")
                .field("ok", &result.is_ok())
                .finish(),
            Self::Retried { row, result } => f
                .debug_struct("Retried")
                .field("row", row)
                .field("ok", &result.is_ok())
                .finish(),
            Self::ActionFailed(failure) => f
                .debug_struct("ActionFailed")
                .field("row", &failure.row)
                .finish(),
            Self::Labels { seq, result } => f
                .debug_struct("Labels")
                .field("seq", seq)
                .field("ok", &result.is_ok())
                .finish(),
            Self::RefreshEnded { seq } => f.debug_struct("RefreshEnded").field("seq", seq).finish(),
            Self::Daemon(event) => {
                let name = match event {
                    DaemonEvent::Connected(_) => "Connected",
                    DaemonEvent::Progress(_) => "Progress",
                    DaemonEvent::Lost(_) => "Lost",
                    DaemonEvent::Refreshed(_) => "Refreshed",
                };
                write!(f, "Daemon({name})")
            }
            Self::Timer { token, kind } => f
                .debug_struct("Timer")
                .field("token", token)
                .field("kind", kind)
                .finish(),
        }
    }
}

/// Send `event` to the screen. A closed channel means the event loop has ended.
pub(crate) fn send(sink: &TaskSink, event: MailEvent) {
    let _ = sink.send(TaskResult::Mail(event));
}

/// A job that waits `delay` and reports `token`. The screen acts only on the newest token.
pub fn timer(delay: Duration, token: u64, kind: Timer) -> Effect {
    Effect::Spawn(Box::new(move |sink| {
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            send(&sink, MailEvent::Timer { token, kind });
        })
    }))
}

/// A job running `work` with the sink.
pub(crate) fn job<F, Fut>(work: F) -> Effect
where
    F: FnOnce(TaskSink) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    Effect::Spawn(Box::new(move |sink| Box::pin(work(sink))))
}

/// A refresh's job: `work` sends the refresh's answers, then `RefreshEnded { seq }` follows
/// whether `work` finished or panicked, so the screen's one-refresh gate always opens again.
/// A panic then goes on to the job's supervisor, which reports it as usual.
pub(crate) fn refresh_job<F, Fut>(seq: u64, work: F) -> Effect
where
    F: FnOnce(TaskSink) -> Fut + Send + 'static,
    Fut: Future<Output = ()> + Send + 'static,
{
    job(move |sink| async move {
        let outcome = AssertUnwindSafe(work(sink.clone())).catch_unwind().await;
        send(&sink, MailEvent::RefreshEnded { seq });
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    })
}
