//! `body.fetch`: bodies the store does not hold yet, fetched by the daemon. The answer is one
//! outcome per message, never a body: the caller reads the body from the store. A front-end
//! operation only: no MCP tool calls it, so agents still cannot read mail.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::OpError;

/// Message ids one `body.fetch` takes at most.
pub const MAX_BODY_FETCH: usize = 50;
/// The longest message id Rail sends.
pub const MAX_MESSAGE_ID_CHARS: usize = 256;

/// `body.fetch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BodyFetchRequest {
    /// An account's name or address.
    pub account: String,
    /// The provider's message ids, at most 50.
    pub message_ids: Vec<String>,
}

/// What happened to one message's body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BodyOutcome {
    /// The store holds the body now (it may have held it already).
    Fetched,
    /// Rail could not read the body; the store holds a marker in its place.
    Unreadable,
    /// The account has no such message, in the store or at the provider.
    Missing,
    /// The fetch failed; retrying may help when the error is transient.
    Failed(OpError),
}

/// One message's outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BodyFetchResult {
    pub message_id: String,
    pub outcome: BodyOutcome,
}

/// The outcomes of one `body.fetch`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BodyFetchResponse {
    /// One per distinct id asked for, in the order asked.
    pub results: Vec<BodyFetchResult>,
}

impl BodyFetchRequest {
    /// The ids, each once, in order. Refused (never quoting an id) when there are more than
    /// `MAX_BODY_FETCH` or one is not 1 to `MAX_MESSAGE_ID_CHARS` ASCII letters, digits, `-`
    /// or `_`.
    pub fn checked_ids(&self) -> Result<Vec<String>, OpError> {
        if self.message_ids.len() > MAX_BODY_FETCH {
            return Err(OpError::invalid_input(format!(
                "body.fetch takes at most {MAX_BODY_FETCH} message ids"
            )));
        }
        let mut seen = HashSet::new();
        let mut ids = Vec::new();
        for id in &self.message_ids {
            let ok = !id.is_empty()
                && id.len() <= MAX_MESSAGE_ID_CHARS
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
            if !ok {
                return Err(OpError::invalid_input(format!(
                    "a message id is 1 to {MAX_MESSAGE_ID_CHARS} letters, digits, '-' or '_'"
                )));
            }
            if seen.insert(id.as_str()) {
                ids.push(id.clone());
            }
        }
        Ok(ids)
    }
}
