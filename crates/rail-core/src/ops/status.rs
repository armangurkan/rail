//! `status` and `accounts.list`.

use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::daemon::DaemonInfo;
use super::error::OpError;
use super::untrusted::Untrusted;
use crate::config::{AccountKind, AiSend};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusResponse {
    pub doctor: DoctorReport,
    pub daemon: DaemonInfo,
}

/// The `rail doctor` checks. Never holds a token value, only presence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorReport {
    pub config_path: PathBuf,
    /// The number of accounts when the config loaded.
    pub config_accounts: Option<usize>,
    /// Why the config did not load: the path and line in `message`, the parser's text (which
    /// can quote the file) in `detail`.
    pub config_error: Option<OpError>,
    pub db_path: PathBuf,
    pub db_size_bytes: Option<u64>,
    pub schema_version: Option<i64>,
    /// SQLite's or the file system's text.
    pub store_error: Option<Untrusted<String>>,
    /// `keyring`, `file` or `1password`.
    pub secrets_backend: String,
    /// Set when the file backend is in use, so the report can warn about it.
    pub secrets_file: Option<PathBuf>,
    /// The token store's text (the OS keyring's message, the 1Password CLI's stderr).
    pub secrets_error: Option<Untrusted<String>>,
    /// False when the probe was skipped because the config did not load.
    pub secrets_checked: bool,
    pub accounts: Vec<AccountStatus>,
}

impl DoctorReport {
    /// True when config, store and secrets all check out. The file-backend warning and accounts
    /// without tokens do not affect health.
    pub fn is_healthy(&self) -> bool {
        self.config_error.is_none()
            && self.store_error.is_none()
            && self.secrets_error.is_none()
            && self.accounts.iter().all(|a| a.token_error.is_none())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountStatus {
    pub name: String,
    pub kind: AccountKind,
    pub address: String,
    pub has_tokens: bool,
    pub token_error: Option<Untrusted<String>>,
    /// Unix seconds.
    pub last_sync_at: Option<i64>,
    pub ai_send: AiSend,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountsListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountsListResponse {
    pub accounts: Vec<AccountSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountSummary {
    pub name: String,
    pub kind: AccountKind,
    pub address: String,
    pub color: Option<String>,
    pub tokens: TokenState,
    /// What an agent may do with this account's mail over MCP.
    pub ai_send: AiSend,
}

/// Whether an account's tokens are stored. Never the tokens themselves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum TokenState {
    Present,
    Missing,
    Error { message: Untrusted<String> },
}
