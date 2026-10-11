//! `sync.status`, `sync.now` and `search`.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::daemon::registry::{Call, Caller};
use crate::ops::{
    OpError, SearchRequest, SearchResponse, SyncNowRequest, SyncNowResponse, SyncStatusRequest,
    SyncStatusResponse, names,
};
use crate::sync::SyncManager;

/// Ids for `sync.progress` subscribers; 0 means none.
static NEXT_WATCH: AtomicU64 = AtomicU64::new(1);

pub(super) async fn status(
    call: Call,
    request: SyncStatusRequest,
) -> Result<SyncStatusResponse, OpError> {
    let accounts = call.ctx.sync.status(request.account.as_deref()).await?;
    Ok(SyncStatusResponse { accounts })
}

/// Subscribe first (when asked), so no progress of the woken pass is missed; a wake that
/// fails takes the subscription back. A connection's first watch is sent every failed action
/// it could not have seen (they happened while it was away), after the live ones start.
pub(super) async fn now(call: Call, request: SyncNowRequest) -> Result<SyncNowResponse, OpError> {
    let watch = request
        .watch
        .then(|| start_watch(&call.ctx.sync, &call.caller));
    match call
        .ctx
        .sync
        .wake(&call.ctx, request.account.as_deref())
        .await
    {
        Ok(targets) => {
            if let Some((_, true)) = watch {
                replay_failures(&call).await;
            }
            Ok(SyncNowResponse { targets })
        }
        Err(error) => {
            if let Some((id, _)) = watch {
                stop_watch(&call.caller, id);
            }
            Err(error)
        }
    }
}

/// One `action.failed` per stored failed row. A live failure sent since the watch began may
/// come again here: the front end lists a row once.
async fn replay_failures(call: &Call) {
    for failure in call.ctx.sync.failed_actions(&call.ctx).await {
        call.caller.notify(names::ACTION_FAILED, &failure);
    }
}

/// Hold the connection's `sync.progress` watch, subscribing when it has none; the watch's
/// id. The subscriber runs under the status board's lock: it only queues a frame on the
/// connection's unbounded channel, never blocks and never calls back into the board (or takes
/// the watch's lock). It drops itself once the connection goes or the watch ended. True with
/// the id when this call subscribed.
fn start_watch(sync: &SyncManager, caller: &Caller) -> (u64, bool) {
    let (id, subscribe) = caller
        .watching()
        .join(NEXT_WATCH.fetch_add(1, Ordering::SeqCst));
    if subscribe {
        let (status_caller, failures) = (caller.clone(), caller.clone());
        sync.subscribe(Box::new(move |status| {
            if !status_caller.is_open() || !status_caller.watching().is_live(id) {
                return false;
            }
            status_caller.notify(names::SYNC_PROGRESS, status);
            true
        }));
        // Under the board's lock too: it only queues a frame, never calls the board.
        sync.subscribe_failures(Box::new(move |failure| {
            if !failures.is_open() || !failures.watching().is_live(id) {
                return false;
            }
            failures.notify(names::ACTION_FAILED, failure);
            true
        }));
    }
    (id, subscribe)
}

/// A request whose wake failed lets go of the watch it held; another request's hold stays.
fn stop_watch(caller: &Caller, id: u64) {
    caller.watching().leave(id);
}

pub(super) async fn search(call: Call, request: SearchRequest) -> Result<SearchResponse, OpError> {
    crate::sync::search::run(&call.ctx, request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AccountKind;
    use crate::ops::{AccountSyncStatus, BrowserEnv};
    use crate::test_support::offline_factory;

    fn setup() -> (
        SyncManager,
        Caller,
        tokio::sync::mpsc::UnboundedSender<Vec<u8>>,
        tokio::sync::mpsc::UnboundedReceiver<Vec<u8>>,
    ) {
        let sync = SyncManager::new(offline_factory());
        sync.board().insert(AccountSyncStatus::new(
            "w",
            "w@example.com",
            AccountKind::Gmail,
        ));
        let (frames, sent) = tokio::sync::mpsc::unbounded_channel();
        let caller = Caller::new(frames.clone(), BrowserEnv::default());
        (sync, caller, frames, sent)
    }

    fn change(sync: &SyncManager, n: u64) {
        sync.board()
            .update("w@example.com", |s| s.passes_completed = n);
    }

    /// Repeated `sync.now { watch: true }` on one connection gives one
    /// notification per change, and a watch every holder let go of sends nothing more.
    #[test]
    fn one_connection_has_one_progress_watch() {
        let (sync, caller, _frames, mut sent) = setup();
        let (first, subscribed) = start_watch(&sync, &caller);
        assert!(subscribed);
        assert_eq!(
            start_watch(&sync, &caller.clone()),
            (first, false),
            "joined, not subscribed"
        );
        change(&sync, 1);
        assert!(sent.try_recv().is_ok());
        assert!(sent.try_recv().is_err(), "one notification per change");
        stop_watch(&caller, first);
        stop_watch(&caller, first);
        change(&sync, 2);
        assert!(sent.try_recv().is_err(), "a watch nobody holds is silent");
        assert_ne!(
            start_watch(&sync, &caller).0,
            first,
            "and can be asked again"
        );
    }

    #[test]
    fn a_watch_also_carries_action_failures() {
        let (sync, caller, _frames, mut sent) = setup();
        let (id, _) = start_watch(&sync, &caller);
        let failure = crate::ops::ActionFailed {
            account: "w".into(),
            row: 1,
            threads: vec![2],
            action: crate::ops::Action::Archive {},
            error: OpError::not_found("gone"),
        };
        sync.board().report_failure(&failure);
        let frame = String::from_utf8(sent.try_recv().unwrap()).unwrap();
        assert!(frame.contains("\"action.failed\""), "{frame}");
        stop_watch(&caller, id);
        sync.board().report_failure(&failure);
        assert!(sent.try_recv().is_err());
    }

    /// Request A's failed wake cannot end the watch request B joined.
    #[test]
    fn a_failed_wake_does_not_cancel_another_requests_watch() {
        let (sync, caller, _frames, mut sent) = setup();
        let (a, _) = start_watch(&sync, &caller);
        let (b, _) = start_watch(&sync, &caller);
        assert_eq!(a, b);
        stop_watch(&caller, a);
        change(&sync, 1);
        assert!(sent.try_recv().is_ok(), "B still watches");
        assert!(sent.try_recv().is_err());
    }
}
