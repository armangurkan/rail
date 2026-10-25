//! `saved_search.put` and `saved_search.remove`: the `[[saved_searches]]` tables of
//! `config.toml`. Only the daemon writes them, and only under the config lock.
//!
//! Step 3 registers the two operations; **step 4** writes the bodies, in the order section 7
//! pins, so no refusal arrives as `ErrorCode::Config`.

use crate::daemon::registry::Call;
use crate::ops::{
    OpError, SavedSearchListResponse, SavedSearchPutRequest, SavedSearchRemoveRequest,
};

pub(super) async fn put(
    _call: Call,
    _request: SavedSearchPutRequest,
) -> Result<SavedSearchListResponse, OpError> {
    Err(OpError::internal("not implemented"))
}

pub(super) async fn remove(
    _call: Call,
    _request: SavedSearchRemoveRequest,
) -> Result<SavedSearchListResponse, OpError> {
    Err(OpError::internal("not implemented"))
}
