//! What the action operations need from the manager: the account an action names, and a
//! nudge after every outbox write (each running worker's depth on the board, and its signal).

use std::sync::Arc;

use tokio::sync::Notify;

use super::SyncManager;
use super::parts::{named, not_syncing, unsupported};
use crate::daemon::DaemonContext;
use crate::ops::{OpError, SyncPhase};
use crate::store::Store;
use crate::sync::FailureSubscriber;
use crate::sync::outbox::publish_depth;

/// A configured Gmail account with a store row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionTarget {
    pub name: String,
    pub address: String,
    pub account_id: i64,
}

impl SyncManager {
    /// The account `who` names (name or address): configured, Gmail, and in the store.
    pub async fn action_target(
        &self,
        ctx: &DaemonContext,
        who: &str,
    ) -> Result<ActionTarget, OpError> {
        self.settled().await;
        let all = self.board.all();
        let status = all
            .iter()
            .find(|s| named(s, who))
            .ok_or_else(|| not_syncing(who))?;
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

    /// `subscriber` runs under the board's lock: it must not block or call the board.
    pub fn subscribe_failures(&self, subscriber: FailureSubscriber) {
        self.board.subscribe_failures(subscriber);
    }
}
