//! Operations that wait on a human: their ids, states and the wait and cancel requests.

use std::fmt;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::accounts::SavedAccount;
use super::error::OpError;
use super::untrusted::Untrusted;

/// The longest one `pending.wait` call blocks.
pub const MAX_WAIT_SECS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PendingId(pub String);

impl fmt::Display for PendingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PendingKind {
    AddAccount,
    Login,
    AdminConsent,
}

/// Where a begun operation stands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PendingState {
    /// Waiting for the user in the browser.
    Waiting,
    /// The login finished and the account is being saved; it cannot be cancelled now.
    Saving,
    Done {
        outcome: PendingOutcome,
    },
    Failed {
        error: OpError,
    },
    Cancelled,
}

impl PendingState {
    /// True once nothing more will happen.
    pub fn is_finished(&self) -> bool {
        matches!(
            self,
            Self::Done { .. } | Self::Failed { .. } | Self::Cancelled
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PendingOutcome {
    Added {
        account: SavedAccount,
    },
    LoggedIn {
        account: SavedAccount,
    },
    /// The tenant Microsoft says consented (outside text); empty if it omitted it.
    Consented {
        tenant: Untrusted<String>,
    },
}

/// `pending.wait`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WaitRequest {
    pub id: PendingId,
    /// At most 300; the default.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

impl WaitRequest {
    /// The bounded wait: the requested seconds, capped at five minutes.
    pub fn timeout(&self) -> Duration {
        Duration::from_secs(
            self.timeout_secs
                .unwrap_or(MAX_WAIT_SECS)
                .min(MAX_WAIT_SECS),
        )
    }
}

/// The answer to `pending.wait` and `pending.cancel`, and the `pending.finished` notification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct WaitResponse {
    pub id: PendingId,
    pub kind: PendingKind,
    pub state: PendingState,
}

/// `pending.cancel`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CancelRequest {
    pub id: PendingId,
}
