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
/// fails takes the subscription back.
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
        Ok(targets) => Ok(SyncNowResponse { targets }),
        Err(error) => {
            if let Some(id) = watch {
                stop_watch(&call.caller, id);
            }
            Err(error)
        }
    }
}

/// Hold the connection's `sync.progress` watch, subscribing when it has none; the watch's
/// id. The subscriber runs under the status board's lock: it only queues a frame on the
/// connection's unbounded channel, never blocks and never calls back into the board (or takes
/// the watch's lock). It drops itself once the connection goes or the watch ended.
fn start_watch(sync: &SyncManager, caller: &Caller) -> u64 {
    let (id, subscribe) = caller
        .watching()
        .join(NEXT_WATCH.fetch_add(1, Ordering::SeqCst));
    if subscribe {
        let caller = caller.clone();
        sync.subscribe(Box::new(move |status| {
            if !caller.is_open() || !caller.watching().is_live(id) {
                return false;
            }
            caller.notify(names::SYNC_PROGRESS, status);
            true
        }));
    }
    id
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
        let first = start_watch(&sync, &caller);
        assert_eq!(
            start_watch(&sync, &caller.clone()),
            first,
            "joined, not subscribed"
        );
        change(&sync, 1);
        assert!(sent.try_recv().is_ok());
        assert!(sent.try_recv().is_err(), "one notification per change");
        stop_watch(&caller, first);
        stop_watch(&caller, first);
        change(&sync, 2);
        assert!(sent.try_recv().is_err(), "a watch nobody holds is silent");
        assert_ne!(start_watch(&sync, &caller), first, "and can be asked again");
    }

    /// Request A's failed wake cannot end the watch request B joined.
    #[test]
    fn a_failed_wake_does_not_cancel_another_requests_watch() {
        let (sync, caller, _frames, mut sent) = setup();
        let a = start_watch(&sync, &caller);
        let b = start_watch(&sync, &caller);
        assert_eq!(a, b);
        stop_watch(&caller, a);
        change(&sync, 1);
        assert!(sent.try_recv().is_ok(), "B still watches");
        assert!(sent.try_recv().is_err());
    }
}
