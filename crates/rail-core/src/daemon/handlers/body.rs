//! `body.fetch`.

use crate::daemon::registry::Call;
use crate::ops::{BodyFetchRequest, BodyFetchResponse, OpError};

pub(super) async fn fetch(
    call: Call,
    request: BodyFetchRequest,
) -> Result<BodyFetchResponse, OpError> {
    crate::sync::body_fetch::run(&call.ctx, request).await
}
