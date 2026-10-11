//! `secrets.configure`.

use crate::auth::migrate::switch_backend;
use crate::daemon::registry::Call;
use crate::ops::{OpError, SecretsConfigureRequest, SecretsConfigureResponse};

/// The whole switch, copy and delete run in one blocking closure under the config lock, so a
/// client leaving half way cannot interrupt it.
pub(super) async fn configure(
    call: Call,
    request: SecretsConfigureRequest,
) -> Result<SecretsConfigureResponse, OpError> {
    let switched = call
        .ctx
        .write_locked(move |ctx| switch_backend(&ctx.paths, &ctx.op_ids, &request))
        .await;
    // The engines' token stores changed: they restart on the new backend.
    if switched.is_ok() {
        call.ctx.sync.reconcile(&call.ctx).await;
    }
    switched
}
