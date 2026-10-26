//! `snooze.set` and `snooze.clear`: a snooze takes threads out of the inbox now and puts them
//! back at a time Rail keeps (neither Gmail nor Microsoft Graph has snooze), and `snooze.woke`
//! says which came back. Terminal-only, like the other actions: no MCP tool reaches them.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::actions::{MAX_ACTION_THREADS, checked_token};
use super::{OpError, OutboxId};

/// The soonest a snooze may end: a minute from now.
pub const MIN_SNOOZE_SECS: i64 = 60;
/// The latest: 366 days from now.
pub const MAX_SNOOZE_SECS: i64 = 366 * 24 * 60 * 60;
/// `snooze.set`'s refusal when none of its threads has mail in the inbox or a snooze to move.
pub const NOT_IN_INBOX: &str = "Only a thread in the inbox can be snoozed.";

/// `snooze.set`: one account's threads, up to 200 (the TUI sends one call per account per 200
/// threads).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnoozeSetRequest {
    /// An account's name or address.
    pub account: String,
    /// Local thread ids (the store's), 1 to 200.
    pub threads: Vec<i64>,
    /// When the threads come back, in Unix seconds.
    pub until: i64,
    /// Wait `UNDO_GRACE` before archiving, so the snooze can be undone.
    #[serde(default)]
    pub grace: bool,
    /// Made by the client; a replay finds the archive it made.
    pub client_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnoozeSetResponse {
    /// The archive's rows, for `action.undo`; none when every thread was snoozed already.
    pub rows: Vec<OutboxId>,
    /// How many of the threads are snoozed now: those with mail in the inbox, or snoozed
    /// already. A thread with neither was left out.
    pub snoozed: usize,
}

/// `snooze.clear`: put these threads back now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SnoozeClearRequest {
    /// An account's name or address.
    pub account: String,
    /// Local thread ids (the store's), 1 to 200.
    pub threads: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnoozeClearResponse {
    /// How many threads went back, or will once their archive is sent.
    pub returned: usize,
}

/// `snooze.woke`: threads (the store's ids) a snooze put back in the inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SnoozeWoke {
    /// The account's name.
    pub account: String,
    pub threads: Vec<i64>,
}

/// Each id once, in order; refused (never quoting an id) unless 1 to `MAX_ACTION_THREADS`
/// positive ids.
fn checked_threads(threads: &[i64]) -> Result<Vec<i64>, OpError> {
    if threads.is_empty() || threads.len() > MAX_ACTION_THREADS {
        return Err(OpError::invalid_input(format!(
            "a snooze takes 1 to {MAX_ACTION_THREADS} threads"
        )));
    }
    if threads.iter().any(|id| *id <= 0) {
        return Err(OpError::invalid_input("a thread id is a positive number"));
    }
    let mut seen = HashSet::new();
    Ok(threads
        .iter()
        .copied()
        .filter(|id| seen.insert(*id))
        .collect())
}

impl SnoozeSetRequest {
    /// The thread ids, each once; refused unless they are sound, the token is safe and `until`
    /// is between a minute and 366 days after `now`.
    ///
    /// # Errors
    /// `invalid_input`, naming what is wrong and never an id.
    pub fn checked(&self, now: i64) -> Result<Vec<i64>, OpError> {
        let threads = checked_threads(&self.threads)?;
        checked_token(&self.client_token)?;
        if self.until < now + MIN_SNOOZE_SECS || self.until > now + MAX_SNOOZE_SECS {
            return Err(OpError::invalid_input(
                "a snooze ends between a minute and a year from now",
            ));
        }
        Ok(threads)
    }
}

impl SnoozeClearRequest {
    /// The thread ids, each once; refused unless they are sound.
    ///
    /// # Errors
    /// `invalid_input`, naming what is wrong and never an id.
    pub fn checked(&self) -> Result<Vec<i64>, OpError> {
        checked_threads(&self.threads)
    }
}
