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
    let watch = if request.watch {
        start_watch(&call.ctx.sync, &call.caller)
    } else {
        None
    };
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

/// Give the connection one `sync.progress` subscriber, unless it has one already; the new
/// subscriber's id, or `None`. The subscriber runs under the status board's lock: it only
/// queues a frame on the connection's unbounded channel, never blocks and never calls back
/// into the board. It drops itself once the connection goes or its id is no longer the
/// connection's.
fn start_watch(sync: &SyncManager, caller: &Caller) -> Option<u64> {
    let id = NEXT_WATCH.fetch_add(1, Ordering::SeqCst);
    caller
        .watching()
        .compare_exchange(0, id, Ordering::SeqCst, Ordering::SeqCst)
        .ok()?;
    let caller = caller.clone();
    sync.subscribe(Box::new(move |status| {
        if !caller.is_open() || caller.watching().load(Ordering::SeqCst) != id {
            return false;
        }
        caller.notify(names::SYNC_PROGRESS, status);
        true
    }));
    Some(id)
}

fn stop_watch(caller: &Caller, id: u64) {
    let _ = caller
        .watching()
        .compare_exchange(id, 0, Ordering::SeqCst, Ordering::SeqCst);
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

    /// Fix round 1: repeated `sync.now { watch: true }` on one connection gives one
    /// notification per change, and a taken-back watch sends nothing more.
    #[test]
    fn one_connection_has_one_progress_watch() {
        let sync = SyncManager::new(offline_factory());
        sync.board().insert(AccountSyncStatus::new(
            "w",
            "w@example.com",
            AccountKind::Gmail,
        ));
        let (frames, mut sent) = tokio::sync::mpsc::unbounded_channel();
        let caller = Caller::new(frames.clone(), BrowserEnv::default());
        let first = start_watch(&sync, &caller).expect("the first watch subscribes");
        assert_eq!(start_watch(&sync, &caller.clone()), None);
        sync.board()
            .update("w@example.com", |s| s.passes_completed = 1);
        assert!(sent.try_recv().is_ok());
        assert!(sent.try_recv().is_err(), "one notification per change");
        stop_watch(&caller, first);
        sync.board()
            .update("w@example.com", |s| s.passes_completed = 2);
        assert!(sent.try_recv().is_err(), "a taken-back watch is silent");
        assert!(
            start_watch(&sync, &caller).is_some(),
            "and can be asked again"
        );
    }
}
