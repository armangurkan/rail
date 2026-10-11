//! Sync status, `sync.status`, `sync.now` and `search`. Status carries counts, phases, times
//! and Rail's own error text only, with the provider's words as untrusted detail: never a
//! subject, sender, snippet or body.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::OpError;
use super::untrusted::Untrusted;
use crate::config::AccountKind;
use crate::error::ErrorClass;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SyncPhase {
    #[default]
    Idle,
    Labels,
    Backfill,
    Delta,
    Bodies,
    Prune,
    /// Waiting before a retry; `next_attempt_at` says until when.
    Retrying,
    /// Paused until the account is logged in again.
    NeedsAuth,
    /// Microsoft 365 sync arrives in a later phase.
    Unsupported,
    Stopped,
}

impl SyncPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Labels => "labels",
            Self::Backfill => "backfill",
            Self::Delta => "delta",
            Self::Bodies => "bodies",
            Self::Prune => "prune",
            Self::Retrying => "retrying",
            Self::NeedsAuth => "needs_auth",
            Self::Unsupported => "unsupported",
            Self::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncErrorInfo {
    /// `transient`, `permanent`, `not_found`, `cursor_expired`, `needs_auth`, `store` or
    /// `start` (the engine could not start).
    pub code: String,
    /// Whether retrying can help; absent from a status that only remembers the code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<ErrorClass>,
    /// Rail's own words.
    pub message: String,
    /// Unix seconds.
    pub at: i64,
    /// The provider's or the system's words.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Untrusted<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AccountSyncStatus {
    pub name: String,
    pub address: String,
    pub kind: AccountKind,
    pub phase: SyncPhase,
    pub needs_auth: bool,
    /// Envelopes the running (or last) backfill fetched.
    pub backfill_fetched: u64,
    /// The mailbox's size as Gmail reports it, during a full backfill.
    pub backfill_estimate: Option<u64>,
    /// Bodies fetched in the running (or last) pass.
    pub bodies_fetched: u64,
    /// What the store holds for the account.
    pub messages: u64,
    pub threads: u64,
    pub bodies: u64,
    /// Unix seconds.
    pub last_success_at: Option<i64>,
    pub last_error: Option<SyncErrorInfo>,
    /// Unix seconds.
    pub next_attempt_at: Option<i64>,
    pub passes_completed: u64,
    pub pass_running: bool,
    pub last_pass_failed: bool,
}

impl AccountSyncStatus {
    pub fn new(name: &str, address: &str, kind: AccountKind) -> Self {
        Self {
            name: name.to_string(),
            address: address.to_ascii_lowercase(),
            kind,
            phase: SyncPhase::Idle,
            needs_auth: false,
            backfill_fetched: 0,
            backfill_estimate: None,
            bodies_fetched: 0,
            messages: 0,
            threads: 0,
            bodies: 0,
            last_success_at: None,
            last_error: None,
            next_attempt_at: None,
            passes_completed: 0,
            pass_running: false,
            last_pass_failed: false,
        }
    }
}

/// The most hits one `search` returns.
pub const MAX_SEARCH_LIMIT: usize = 100;
/// The hits a `search` without a limit returns.
pub const DEFAULT_SEARCH_LIMIT: usize = 20;
/// The longest query Rail sends to Gmail.
pub const MAX_QUERY_CHARS: usize = 1024;

fn default_limit() -> usize {
    DEFAULT_SEARCH_LIMIT
}

/// `sync.status`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncStatusRequest {
    /// An account's name or address; every account when absent.
    #[serde(default)]
    pub account: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncStatusResponse {
    pub accounts: Vec<AccountSyncStatus>,
}

/// `sync.now`: wake one account, or all.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SyncNowRequest {
    #[serde(default)]
    pub account: Option<String>,
    /// Send `sync.progress` notifications on this connection from now on.
    #[serde(default)]
    pub watch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncTarget {
    pub name: String,
    pub address: String,
    /// The pass this request asked for has ended once the account's `passes_completed`
    /// reaches this.
    pub pass: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SyncNowResponse {
    pub targets: Vec<SyncTarget>,
}

/// The arguments of the `rail_sync_now` MCP tool: which account to wake. No way to subscribe.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AgentSyncNow {
    /// An account's name or address; every account when absent.
    #[serde(default)]
    pub account: Option<String>,
}

impl AgentSyncNow {
    pub fn into_request(self) -> SyncNowRequest {
        SyncNowRequest {
            account: self.account,
            watch: false,
        }
    }
}

/// `search`: the store's full-text index and Gmail's own search, merged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchRequest {
    pub query: String,
    /// An account's name or address; every account when absent.
    #[serde(default)]
    pub account: Option<String>,
    /// At most 100.
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HitSource {
    /// Found in the store.
    Local,
    /// Found by the provider's search and fetched into the store.
    Server,
}

/// One thread a search found. `sender` and `subject` are mail content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub account: String,
    pub thread_id: String,
    /// The newest matching message's receive time, Unix seconds.
    pub date: i64,
    pub sender: Untrusted<String>,
    pub subject: Untrusted<String>,
    pub source: HitSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchFailure {
    pub account: String,
    pub error: OpError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SearchResponse {
    pub hits: Vec<SearchHit>,
    /// Accounts whose server search failed; their local hits are still in `hits`.
    pub failures: Vec<SearchFailure>,
}
