//! `secrets.configure`: choose where tokens live and optionally move them.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::yes;
use crate::config::SecretsBackend;

/// There is deliberately no field for the `op` binary: which program Rail runs is chosen only
/// by the user, by hand, in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretsConfigureRequest {
    /// `keyring`, `file` or `1password`.
    pub backend: SecretsBackend,
    /// 1Password only: the vault Rail keeps its items in. Required unless config already has one.
    #[serde(default)]
    pub vault: Option<String>,
    /// 1Password only: the account, when several are signed in.
    #[serde(default)]
    pub op_account: Option<String>,
    /// Copy every account's tokens to the new backend, check they read back, then delete the
    /// old copies. A failure part way leaves the old copies and the config as they were.
    #[serde(default = "yes")]
    pub migrate: bool,
    /// Set only by a trusted front end after the user confirmed the change (see
    /// `auth::migrate::describe_switch` for the text to show). The daemon refuses any request
    /// that changes the backend, vault or account without it. Never offered to an agent.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SecretsConfigureResponse {
    pub backend: SecretsBackend,
    pub previous: SecretsBackend,
    /// Addresses whose tokens were copied to the new backend.
    pub moved: Vec<String>,
    /// Addresses whose old copy could not be deleted after the move.
    pub old_copies_kept: Vec<String>,
    pub config_path: PathBuf,
}
