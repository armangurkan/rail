//! `sync.status`, `sync.now` and `search`.

use crate::daemon::registry::Call;
use crate::ops::{
    OpError, SearchRequest, SearchResponse, SyncNowRequest, SyncNowResponse, SyncStatusRequest,
    SyncStatusResponse, names,
};

pub(super) async fn status(
    call: Call,
    request: SyncStatusRequest,
) -> Result<SyncStatusResponse, OpError> {
    let accounts = call.ctx.sync.status(request.account.as_deref()).await?;
    Ok(SyncStatusResponse { accounts })
}

/// Subscribe first (when asked), so no progress of the woken pass is missed. The subscriber
/// runs under the status board's lock: it only queues a frame on the connection's unbounded
/// channel, never blocks and never calls back into the board.
pub(super) async fn now(call: Call, request: SyncNowRequest) -> Result<SyncNowResponse, OpError> {
    if request.watch {
        let caller = call.caller.clone();
        call.ctx.sync.subscribe(Box::new(move |status| {
            if !caller.is_open() {
                return false;
            }
            caller.notify(names::SYNC_PROGRESS, status);
            true
        }));
    }
    let targets = call.ctx.sync.wake(request.account.as_deref()).await?;
    Ok(SyncNowResponse { targets })
}

pub(super) async fn search(call: Call, request: SearchRequest) -> Result<SearchResponse, OpError> {
    crate::sync::search::run(&call.ctx, request).await
}
