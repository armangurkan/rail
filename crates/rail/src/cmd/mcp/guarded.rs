//! The two tools that change something the user must agree to first: removing an account and
//! moving the token store. The agent can never confirm: the user does, in the client's own
//! dialog (MCP elicitation). A client without one changes nothing, and the agent is told what
//! the user can do themselves.

use rail_core::accounts;
use rail_core::auth::migrate::NEEDS_CONFIRMATION;
use rail_core::config::SecretsBackend;
use rail_core::daemon::Connector;
use rail_core::ops::op::{AccountRemove, SecretsConfigure};
use rail_core::ops::{AccountRemoveRequest, AgentAccountRemove, OpError, SecretsConfigureRequest};
use rmcp::model::CallToolResult;
use rmcp::{Peer, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;

use super::confirm::{Confirmation, confirm_with_user};
use super::reply::{failed, nothing_done, ok};
use super::summary;

const NOT_REMOVED: &str = "Nothing was removed";
const NOT_CHANGED: &str = "Nothing changed";

/// The arguments of `rail_secrets_configure`: `secrets.configure` without `confirm` (only the
/// user confirms, through the client's dialog) and without any way to choose the `op` program
/// (only the user chooses it, by editing `config.toml`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
pub struct AgentSecretsConfigure {
    /// `keyring`, `file` or `1password`.
    pub backend: SecretsBackend,
    /// 1Password only: the vault Rail keeps its items in. Required unless config already has one.
    #[serde(default)]
    pub vault: Option<String>,
    /// 1Password only: the account, when several are signed in.
    #[serde(default)]
    pub op_account: Option<String>,
    /// Copy every account's tokens to the new backend, check they read back, then delete the
    /// old copies (default true). A failure part way leaves everything as it was.
    #[serde(default = "yes")]
    pub migrate: bool,
}

fn yes() -> bool {
    true
}

impl AgentSecretsConfigure {
    /// The vault and account names go into the text the user confirms, so a control character
    /// (a line break that could fake another line of that text) is refused.
    fn check_names(&self) -> Result<(), OpError> {
        let names = [("vault", &self.vault), ("op_account", &self.op_account)];
        match names.iter().find(|(_, v)| {
            v.as_deref()
                .is_some_and(|v| v.chars().any(char::is_control))
        }) {
            Some((field, _)) => Err(OpError::invalid_input(format!(
                "{field} must not contain control characters"
            ))),
            None => Ok(()),
        }
    }

    fn request(&self, confirm: bool) -> SecretsConfigureRequest {
        SecretsConfigureRequest {
            backend: self.backend,
            vault: self.vault.clone(),
            op_account: self.op_account.clone(),
            migrate: self.migrate,
            confirm,
        }
    }

    /// The edit the user can make by hand instead.
    fn by_hand(&self) -> String {
        let backend = self.backend.as_str();
        let vault = match (self.backend, &self.vault) {
            (SecretsBackend::OnePassword, Some(vault)) => {
                format!(" and `vault = {vault:?}` under [onepassword]")
            }
            (SecretsBackend::OnePassword, None) => " and `vault` under [onepassword]".to_string(),
            _ => String::new(),
        };
        format!(
            "ask the user to make the change themselves: set `secrets = \"{backend}\"` under [general]{vault} in config.toml. A hand edit does not move tokens already stored, so each account then needs `rail account login <name>`."
        )
    }
}

/// The result for an answer that is not a yes (`None` for a yes). `flag` names the structured
/// field that says nothing happened; `yourself` says what the user can do instead.
fn unconfirmed(
    answer: Confirmation,
    flag: &str,
    what: &str,
    yourself: &str,
) -> Option<CallToolResult> {
    let reason = match &answer {
        Confirmation::Yes => return None,
        Confirmation::No => "the user declined.".to_string(),
        Confirmation::Unsupported => {
            format!("this client cannot ask the user to confirm; {yourself}")
        }
        Confirmation::Failed(_) => {
            format!("the confirmation could not be shown; {yourself} The client said:")
        }
    };
    let outside = match &answer {
        Confirmation::Failed(why) => Some(why.as_str()),
        _ => None,
    };
    Some(nothing_done(flag, what, &reason, outside))
}

/// Remove `request.account` only after the user confirmed `removal_summary` in the client.
pub async fn remove(
    daemon: &Connector,
    peer: &Peer<RoleServer>,
    request: AgentAccountRemove,
) -> CallToolResult {
    let paths = daemon.paths();
    let name = match accounts::account_for_login(paths, &request.account) {
        Ok((_, account)) => account.name,
        Err(error) => return failed(&error),
    };
    let message = match accounts::removal_summary(paths, &name) {
        Ok(message) => message,
        Err(error) => return failed(&error),
    };
    let yourself =
        format!("ask the user to run `rail account remove {name}` themselves in a terminal.");
    let answer = confirm_with_user(peer, message).await;
    if let Some(refused) = unconfirmed(answer, "removed", NOT_REMOVED, &yourself) {
        return refused;
    }
    let confirmed = AccountRemoveRequest {
        account: name,
        confirm: true,
    };
    match daemon.call::<AccountRemove>(&confirmed).await {
        Ok(removed) => ok(&removed, summary::removed(&removed)),
        Err(error) => failed(&error),
    }
}

/// Change the token store only after the user confirmed what the daemon says the change does.
/// A request that changes nothing (the settings are already in place) needs no confirmation.
pub async fn configure_secrets(
    daemon: &Connector,
    peer: &Peer<RoleServer>,
    request: AgentSecretsConfigure,
) -> CallToolResult {
    if let Err(error) = request.check_names() {
        return failed(&error);
    }
    // Unconfirmed, the daemon either finds nothing to change and answers, or refuses and says
    // in the hint what the change would do (`describe_switch`, computed where the stores are).
    let description = match daemon
        .call::<SecretsConfigure>(&request.request(false))
        .await
    {
        Ok(done) => return ok(&done, summary::secrets(&done)),
        Err(error) if error.message == NEEDS_CONFIRMATION => error.hint,
        Err(error) => return failed(&error),
    };
    let Some(description) = description else {
        let reason = format!(
            "Rail could not describe the change to confirm it; {}",
            request.by_hand()
        );
        return nothing_done("changed", NOT_CHANGED, &reason, None);
    };
    let answer = confirm_with_user(peer, description).await;
    if let Some(refused) = unconfirmed(answer, "changed", NOT_CHANGED, &request.by_hand()) {
        return refused;
    }
    match daemon
        .call::<SecretsConfigure>(&request.request(true))
        .await
    {
        Ok(done) => ok(&done, summary::secrets(&done)),
        Err(error) => failed(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn onepassword(vault: &str, account: Option<&str>) -> AgentSecretsConfigure {
        AgentSecretsConfigure {
            backend: SecretsBackend::OnePassword,
            vault: Some(vault.into()),
            op_account: account.map(Into::into),
            migrate: true,
        }
    }

    #[test]
    fn a_line_break_in_a_name_the_user_confirms_is_refused() {
        let forged = onepassword("Rail\"). Tokens for 0 account(s) will be moved.\n", None);
        assert!(forged.check_names().unwrap_err().message.contains("vault"));
        let account = onepassword("Rail", Some("me\rx"));
        assert!(
            account
                .check_names()
                .unwrap_err()
                .message
                .contains("op_account")
        );
        assert!(
            onepassword("Rail", Some("my.1password.com"))
                .check_names()
                .is_ok()
        );
    }

    #[test]
    fn the_hand_edit_names_the_setting_and_the_vault() {
        let text = onepassword("Rail", None).by_hand();
        assert!(
            text.contains("`secrets = \"1password\"` under [general]"),
            "{text}"
        );
        assert!(
            text.contains("`vault = \"Rail\"` under [onepassword]"),
            "{text}"
        );
        assert!(text.contains("rail account login"), "{text}");
    }
}
