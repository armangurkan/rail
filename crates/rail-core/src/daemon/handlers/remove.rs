//! `account.remove`. Nothing happens without `confirm: true` and the digest of the plan the
//! person was shown, which only a trusted front end sends after the person confirmed:
//! `rail account remove` on an interactive terminal, and the MCP server after the user
//! accepted its elicitation form. No agent-settable path exists. Any other request is refused
//! with the text to show and the digest to send back, and changes nothing.

use crate::accounts::remove_confirmed;
use crate::daemon::registry::Call;
use crate::ops::{AccountRemoveRequest, AccountRemoveResponse, OpError};

pub(super) async fn remove(
    call: Call,
    request: AccountRemoveRequest,
) -> Result<AccountRemoveResponse, OpError> {
    call.ctx
        .write_locked(move |ctx| {
            let plan = request.plan.as_deref().filter(|_| request.confirm);
            remove_confirmed(&ctx.paths, &ctx.op_ids, &request.account, plan)
        })
        .await
}
