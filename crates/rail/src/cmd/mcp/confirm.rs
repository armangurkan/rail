//! Asking the user, through the MCP client, to confirm an action. `account.remove` and
//! `secrets.configure` use it now; the send, reply, forward and delete tools will use it under
//! the `confirm` send policy.

use rmcp::service::ElicitationError;
use rmcp::{Peer, RoleServer};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The one field the confirmation form asks for.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct Confirm {
    /// Tick to go ahead.
    pub confirm: bool,
}

rmcp::elicit_safe!(Confirm);

#[derive(Debug, PartialEq, Eq)]
pub enum Confirmation {
    /// The user accepted and ticked the box.
    Yes,
    /// The user declined, dismissed the form, or left the box unticked.
    No,
    /// The client cannot show a form; the caller decides what that means.
    Unsupported,
    /// The request failed; nothing should happen. The text is rmcp's, and may quote what the
    /// client sent: it is outside text.
    Failed(String),
}

/// Show `message` with a confirm box and wait for the user.
pub async fn confirm_with_user(peer: &Peer<RoleServer>, message: String) -> Confirmation {
    match peer.elicit::<Confirm>(message).await {
        Ok(Some(Confirm { confirm: true })) => Confirmation::Yes,
        Ok(_) | Err(ElicitationError::UserDeclined | ElicitationError::UserCancelled) => {
            Confirmation::No
        }
        Err(ElicitationError::CapabilityNotSupported) => Confirmation::Unsupported,
        Err(e) => {
            // The error's text can quote the client's answer; log only that it failed.
            tracing::warn!("a confirmation request to the MCP client failed");
            Confirmation::Failed(e.to_string())
        }
    }
}
