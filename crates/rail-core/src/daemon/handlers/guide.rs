//! `guide.provider`.

use crate::daemon::registry::Call;
use crate::guide::guide;
use crate::ops::{GuideRequest, GuideResponse, OpError};

pub(super) async fn provider(_: Call, request: GuideRequest) -> Result<GuideResponse, OpError> {
    guide(&request)
}
