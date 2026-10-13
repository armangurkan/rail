//! What the action operations need from the manager: the account an action names, and a
//! nudge after every outbox write (each running worker's depth on the board, and its signal).

use std::sync::Arc;

use tokio::sync::Notify;

use super::SyncManager;
use super::parts::{m365_labels, named, not_syncing, unsupported};
use crate::config::AccountKind;
use crate::daemon::DaemonContext;
use crate::error::ErrorClass;
use crate::ops::{Action, ActionFailed, ErrorCode, OpError, SyncPhase};
use crate::store::Store;
use crate::store::outbox::OutboxRow;
use crate::sync::FailureSubscriber;
use crate::sync::outbox::publish_depth;

/// A configured account with a store row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionTarget {
    pub name: String,
    pub address: String,
    pub account_id: i64,
    /// The account's provider: its name in what the handlers say.
    pub kind: AccountKind,
}

/// What a caller wants the account for: one action, or writing mail (save, send, push,
/// import).
#[derive(Debug, Clone, Copy)]
pub enum Wants<'a> {
    Action(&'a Action),
    Compose,
}

impl Wants<'_> {
    /// Whether a Microsoft 365 account refuses it at once: a labels action (Outlook has
    /// folders, and `m` moves to one). Every other action and all writing reach it.
    fn refused_on_m365(self) -> bool {
        matches!(self, Self::Action(Action::Labels { .. }))
    }
}

impl SyncManager {
    /// The account `who` names (name or address): configured, syncing, in the store, and able
    /// to do what `wants` asks. A Microsoft 365 account refuses labels at once.
    pub async fn action_target(
        &self,
        ctx: &DaemonContext,
        who: &str,
        wants: Wants<'_>,
    ) -> Result<ActionTarget, OpError> {
        self.settled().await;
        let all = self.board.all();
        let status = all
            .iter()
            .find(|s| named(s, who))
            .ok_or_else(|| not_syncing(who))?;
        if status.kind == AccountKind::M365 && wants.refused_on_m365() {
            return Err(m365_labels());
        }
        if status.phase == SyncPhase::Unsupported {
            return Err(unsupported());
        }
        // The writer opens (and migrates) the store first.
        drop(self.writer(ctx).await?);
        let (db, address) = (ctx.paths.db_file(), status.address.clone());
        let id = ctx
            .blocking(move || Ok(Store::open_reader(&db)?.account_id(&address)?))
            .await?;
        let account_id =
            id.ok_or_else(|| OpError::not_found("this account has no mail in the store yet"))?;
        Ok(ActionTarget {
            name: status.name.clone(),
            address: status.address.clone(),
            account_id,
            kind: status.kind,
        })
    }

    /// After an outbox write: publish each running account's depth and wake its worker.
    pub async fn outbox_changed(&self, ctx: &DaemonContext) {
        let running: Vec<(String, i64, Arc<Notify>)> = {
            let engines = self.engines.lock().await;
            engines
                .running
                .iter()
                .map(|(a, r)| (a.clone(), r.account_id, r.outbox_signal.clone()))
                .collect()
        };
        let (db, board) = (ctx.paths.db_file(), self.board.clone());
        let accounts: Vec<(String, i64)> =
            running.iter().map(|(a, id, _)| (a.clone(), *id)).collect();
        let published = ctx
            .blocking(move || {
                let store = Store::open_reader(&db)?;
                for (address, id) in &accounts {
                    publish_depth(&board, address, &store, *id);
                }
                Ok(())
            })
            .await;
        if let Err(error) = published {
            tracing::warn!(error = %error.message, "could not publish the outbox depth");
        }
        for (_, _, signal) in running {
            signal.notify_one();
        }
    }

    /// Every account's failed rows (at most `MAX_FAILED_ROWS` each) as the `action.failed`
    /// notifications a new watcher missed. A store that cannot be read gives none (logged).
    pub async fn failed_actions(&self, ctx: &DaemonContext) -> Vec<ActionFailed> {
        let accounts: Vec<(String, String, AccountKind)> = self
            .board
            .all()
            .iter()
            .map(|s| (s.name.clone(), s.address.clone(), s.kind))
            .collect();
        let db = ctx.paths.db_file();
        let read = ctx
            .blocking(move || {
                let store = Store::open_reader(&db)?;
                let mut failures = Vec::new();
                for (name, address, kind) in &accounts {
                    if let Some(id) = store.account_id(address)? {
                        let rows = store.failed_rows(id)?;
                        failures
                            .extend(rows.into_iter().map(|row| failed_notice(name, *kind, row)));
                    }
                }
                Ok(failures)
            })
            .await;
        read.unwrap_or_else(|error| {
            tracing::warn!(error = %error.message, "could not read the failed actions");
            Vec::new()
        })
    }

    /// `subscriber` runs under the board's lock: it must not block or call the board.
    pub fn subscribe_failures(&self, subscriber: FailureSubscriber) {
        self.board.subscribe_failures(subscriber);
    }
}

/// A stored failure as its notification: `last_error` is Rail's words, then (after the first
/// `": "`) the provider's text, kept apart as the detail; a stored failure is final.
fn failed_notice(account: &str, kind: AccountKind, row: OutboxRow) -> ActionFailed {
    let last = row.last_error.unwrap_or_default();
    let (words, detail) = match last.split_once(": ") {
        Some((words, detail)) => (words.to_string(), Some(detail.to_string())),
        None => (last, None),
    };
    let words = if words.is_empty() {
        format!("{} refused the change", kind.provider_name())
    } else {
        words
    };
    let mut error = OpError::new(ErrorCode::Provider, ErrorClass::Permanent, words);
    if let Some(detail) = detail {
        error = error.with_detail(detail);
    }
    ActionFailed {
        account: account.to_string(),
        row: row.id,
        threads: row.thread_ids,
        draft: row.action.draft_id(),
        action: row.action,
        error,
        client_token: row.client_token,
    }
}
