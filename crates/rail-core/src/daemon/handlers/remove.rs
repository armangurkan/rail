//! `account.remove`. Nothing happens without `confirm: true`, which only a trusted front end
//! sets after a person confirmed: `rail account remove` on an interactive terminal, and the
//! MCP server after the user accepted its elicitation form. No agent-settable path exists.

use crate::accounts::remove_account;
use crate::daemon::registry::Call;
use crate::error::ErrorClass;
use crate::ops::{AccountRemoveRequest, AccountRemoveResponse, ErrorCode, OpError};

pub(super) async fn remove(
    call: Call,
    request: AccountRemoveRequest,
) -> Result<AccountRemoveResponse, OpError> {
    if !request.confirm {
        return Err(OpError::new(
            ErrorCode::NotConfirmed,
            ErrorClass::Permanent,
            "removing an account needs the user's confirmation",
        )
        .with_hint(
            "nothing was removed; the user confirms in Rail's own terminal or dialog, and their mail stays",
        ));
    }
    call.ctx
        .write_locked(move |ctx| remove_account(&ctx.paths, &ctx.op_ids, &request.account))
        .await
}
