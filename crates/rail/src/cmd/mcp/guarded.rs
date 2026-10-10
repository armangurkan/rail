//! The two tools that change something the user must agree to first: removing an account and
//! moving the token store. The agent can never confirm: the user does, in the client's own
//! dialog (MCP elicitation). A client without one changes nothing, and the agent is told what
//! the user can do themselves.

use rail_core::config::SecretsBackend;
use rail_core::daemon::Connector;
use rail_core::ops::op::{AccountRemove, SecretsConfigure};
use rail_core::ops::{
    AccountRemoveRequest, AgentAccountRemove, ConfirmPlan, ErrorCode, OpError,
    SecretsConfigureRequest,
};
use rmcp::model::CallToolResult;
use rmcp::{Peer, RoleServer};
use schemars::JsonSchema;
use serde::Deserialize;

use super::confirm::{Confirmation, confirm_with_user};
use super::reply::{failed, nothing_done, ok};
use super::summary;

const NOT_REMOVED: &str = "Nothing was removed";
const NOT_CHANGED: &str = "Nothing changed";
/// Said when the daemon refuses a confirmed request because its plan changed meanwhile.
const PLAN_CHANGED: &str = "what the user confirmed no longer matches what would run (something changed meanwhile, such as an account added), so nothing ran; call the tool again so the user can confirm the new plan.";

/// The arguments of `rail_secrets_configure`: `secrets.configure` without `confirm` (only the
/// user confirms, through the client's dialog) and without any way to choose the `op` program
/// (only the user chooses it, by editing `config.toml`).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
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

    /// The daemon request: unconfirmed (`plan` absent) to learn the plan, or confirmed with
    /// the plan's digest after the user said yes.
    fn request(&self, plan: Option<String>) -> SecretsConfigureRequest {
        SecretsConfigureRequest {
            backend: self.backend,
            vault: self.vault.clone(),
            op_account: self.op_account.clone(),
            migrate: self.migrate,
            confirm: plan.is_some(),
            plan,
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
            "ask the user to make the change themselves (do not edit config.toml yourself): set `secrets = \"{backend}\"` under [general]{vault} in config.toml. A hand edit does not move tokens already stored, so each account then needs `rail account login <name>`."
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

/// What the daemon said to an unconfirmed request.
enum Probe<T> {
    /// Nothing needed confirming; this is the answer.
    Done(T),
    /// The text to show the user and the plan digest to send back.
    Ask(ConfirmPlan),
    Failed(OpError),
}

fn probe<T>(answer: Result<T, OpError>) -> Probe<T> {
    match answer {
        Ok(done) => Probe::Done(done),
        Err(error) if error.code == ErrorCode::NotConfirmed => match error.confirmation {
            Some(plan) => Probe::Ask(*plan),
            None => Probe::Failed(error),
        },
        Err(error) => Probe::Failed(error),
    }
}

/// The result of a confirmed request: the daemon refuses one whose plan changed after the
/// user confirmed it, and nothing ran.
fn confirmed<T: serde::Serialize>(
    answer: Result<T, OpError>,
    flag: &str,
    what: &str,
    text: impl FnOnce(&T) -> String,
) -> CallToolResult {
    match answer {
        Ok(done) => {
            let summary = text(&done);
            ok(&done, summary)
        }
        Err(error) if error.code == ErrorCode::NotConfirmed => {
            nothing_done(flag, what, PLAN_CHANGED, None)
        }
        Err(error) => failed(&error),
    }
}

/// `who` as the user would type it in a shell: as is when it is a plain name or address.
fn as_typed(who: &str) -> String {
    let plain = !who.is_empty()
        && who
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@._+-".contains(c));
    if plain {
        who.to_string()
    } else {
        format!("{who:?}")
    }
}

/// Remove `request.account` only after the user confirmed, in the client, the text the daemon
/// gave for it; the confirmed request carries the daemon's plan digest.
pub async fn remove(
    daemon: &Connector,
    peer: &Peer<RoleServer>,
    request: AgentAccountRemove,
) -> CallToolResult {
    let mut call = AccountRemoveRequest {
        account: request.account,
        confirm: false,
        plan: None,
    };
    let shown = match probe(daemon.call::<AccountRemove>(&call).await) {
        Probe::Done(removed) => return ok(&removed, summary::removed(&removed)),
        Probe::Ask(shown) => shown,
        Probe::Failed(error) => return failed(&error),
    };
    let yourself = format!(
        "ask the user to run `rail account remove {}` themselves in a terminal.",
        as_typed(&call.account)
    );
    let answer = confirm_with_user(peer, shown.text).await;
    if let Some(refused) = unconfirmed(answer, "removed", NOT_REMOVED, &yourself) {
        return refused;
    }
    call.confirm = true;
    call.plan = Some(shown.plan);
    let answer = daemon.call::<AccountRemove>(&call).await;
    confirmed(answer, "removed", NOT_REMOVED, summary::removed)
}

/// Change the token store only after the user confirmed, in the client, the text the daemon
/// gave for the change; the confirmed request carries the daemon's plan digest. A request that
/// changes nothing (the settings are already in place) needs no confirmation.
pub async fn configure_secrets(
    daemon: &Connector,
    peer: &Peer<RoleServer>,
    request: AgentSecretsConfigure,
) -> CallToolResult {
    if let Err(error) = request.check_names() {
        return failed(&error);
    }
    let shown = match probe(
        daemon
            .call::<SecretsConfigure>(&request.request(None))
            .await,
    ) {
        Probe::Done(done) => return ok(&done, summary::secrets(&done)),
        Probe::Ask(shown) => shown,
        Probe::Failed(error) => return failed(&error),
    };
    let answer = confirm_with_user(peer, shown.text).await;
    if let Some(refused) = unconfirmed(answer, "changed", NOT_CHANGED, &request.by_hand()) {
        return refused;
    }
    let answer = daemon
        .call::<SecretsConfigure>(&request.request(Some(shown.plan)))
        .await;
    confirmed(answer, "changed", NOT_CHANGED, summary::secrets)
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
        assert!(text.contains("do not edit config.toml yourself"), "{text}");
    }

    #[test]
    fn a_name_is_shown_as_the_user_would_type_it() {
        assert_eq!(as_typed("work"), "work");
        assert_eq!(as_typed("me@example.com"), "me@example.com");
        assert_eq!(as_typed("a b; rm -rf ~"), "\"a b; rm -rf ~\"");
    }
}
