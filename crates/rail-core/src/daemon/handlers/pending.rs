//! `pending.wait` and `pending.cancel`.

use crate::daemon::registry::Call;
use crate::ops::{CancelRequest, OpError, PendingId, WaitRequest, WaitResponse};

pub(super) async fn wait(call: Call, request: WaitRequest) -> Result<WaitResponse, OpError> {
    call.ctx
        .pending
        .wait(&request.id, request.timeout())
        .await
        .ok_or_else(|| unknown(&request.id))
}

pub(super) async fn cancel(call: Call, request: CancelRequest) -> Result<WaitResponse, OpError> {
    call.ctx
        .pending
        .cancel(&request.id)
        .ok_or_else(|| unknown(&request.id))
}

fn unknown(id: &PendingId) -> OpError {
    OpError::not_found(format!("no pending operation {id}"))
        .with_hint("ids are kept for ten minutes after they finish; begin again")
}
