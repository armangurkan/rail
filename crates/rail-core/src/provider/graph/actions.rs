//! Thread changes for Microsoft 365. The scope rule (`actions::scope`) picks each thread's
//! messages from the facts the worker hands over; each picked message is one `$batch` item:
//! archive, trash and move are `POST me/messages/{id}/move` to the Archive, Deleted Items or
//! chosen folder (checked against the last folder listing), star and read are `PATCH
//! me/messages/{id}`. Each message's answer stands alone (#30): done, gone (404) or refused
//! (logged by id and skipped); a 401 or a transient answer is the whole call's, and the call
//! fails only when Graph refused every message. Labels never get here (Outlook has folders).

use serde_json::{Value, json};

use super::batch::{BatchAnswer, BatchRequest};
use super::{GraphProvider, graph_id};
use crate::actions::scope::touched;
use crate::ops::{M365_LABELS, M365_NO_ARCHIVE};
use crate::provider::{Change, FolderKind, ProviderError, RemoteId, ThreadTarget};

/// A move into a folder Graph no longer lists.
pub const MOVE_TARGET_GONE: &str = "the folder for this move no longer exists in Microsoft 365";
const NO_DELETED_ITEMS: &str = "This mailbox has no Deleted Items folder.";

fn unsendable(what: &str) -> ProviderError {
    ProviderError::permanent(&format!(
        "a {what} id is not one Rail sends to Microsoft 365"
    ))
}

/// The PATCH body of a flag or read change; `None` for a move.
fn patch_body(change: &Change) -> Option<Value> {
    match change {
        Change::Read { on } => Some(json!({"isRead": on})),
        Change::Star { on } => {
            let status = if *on { "flagged" } else { "notFlagged" };
            Some(json!({"flag": {"flagStatus": status}}))
        }
        Change::Archive | Change::Trash | Change::Move { .. } | Change::Labels { .. } => None,
    }
}

/// The ids of the messages the scope rule picks, thread by thread.
fn touched_ids(threads: &[ThreadTarget], change: &Change) -> Vec<RemoteId> {
    threads
        .iter()
        .flat_map(|t| {
            touched(change, &t.messages)
                .into_iter()
                .map(|i| t.messages[i].id.clone())
        })
        .collect()
}

/// Each touched message's answer: a 2xx is done and a 404 is gone (skipped); a 401 or a
/// transient answer is the whole call's; any other refusal is skipped and logged by id. The
/// call fails only when Graph refused every message, with its first refusal.
fn settle(ids: &[RemoteId], answers: Vec<BatchAnswer>) -> Result<(), ProviderError> {
    let mut first: Option<ProviderError> = None;
    let mut refused = 0;
    for (id, answer) in ids.iter().zip(answers) {
        let status = answer.status;
        match answer.into_body() {
            Ok(_) => {}
            Err(error @ (ProviderError::NeedsAuth { .. } | ProviderError::Transient { .. })) => {
                return Err(error);
            }
            Err(error) => {
                tracing::warn!(id = %id, status, class = error.code(), "Microsoft 365 refused one message of an action; skipped");
                refused += 1;
                first.get_or_insert(error);
            }
        }
    }
    match first {
        Some(error) if refused == ids.len() => Err(error),
        _ => Ok(()),
    }
}

impl GraphProvider {
    /// `change` on the messages the scope rule picks from each target. Nothing to touch sends
    /// nothing, not even a folder listing.
    pub(crate) async fn apply_change(
        &self,
        threads: &[ThreadTarget],
        change: &Change,
    ) -> Result<(), ProviderError> {
        if matches!(change, Change::Labels { .. }) {
            return Err(ProviderError::permanent(M365_LABELS));
        }
        let ids = touched_ids(threads, change);
        if ids.is_empty() {
            return Ok(());
        }
        if !ids.iter().all(|id| graph_id(id)) {
            return Err(unsendable("message"));
        }
        let requests = self.requests_for(&ids, change).await?;
        let answers = self.batch(&requests).await?;
        settle(&ids, answers)
    }

    async fn requests_for(
        &self,
        ids: &[RemoteId],
        change: &Change,
    ) -> Result<Vec<BatchRequest>, ProviderError> {
        if let Some(body) = patch_body(change) {
            return Ok(ids
                .iter()
                .map(|id| BatchRequest::patch(format!("/me/messages/{id}"), body.clone()))
                .collect());
        }
        let body = json!({"destinationId": self.destination(change).await?});
        Ok(ids
            .iter()
            .map(|id| BatchRequest::post(format!("/me/messages/{id}/move"), body.clone()))
            .collect())
    }

    /// Where archive, trash or a move sends messages, from the last folder listing.
    async fn destination(&self, change: &Change) -> Result<RemoteId, ProviderError> {
        let folders = self.known_folders().await?;
        let of_kind = |kind: FolderKind, missing: &str| {
            folders
                .iter()
                .find(|f| f.kind == kind)
                .map(|f| f.remote_id.clone())
                .ok_or_else(|| ProviderError::permanent(missing))
        };
        let id = match change {
            Change::Archive => of_kind(FolderKind::Archive, M365_NO_ARCHIVE)?,
            Change::Trash => of_kind(FolderKind::Trash, NO_DELETED_ITEMS)?,
            Change::Move { to } if folders.iter().any(|f| &f.remote_id == to) => to.clone(),
            Change::Move { .. } => return Err(ProviderError::permanent(MOVE_TARGET_GONE)),
            Change::Star { .. } | Change::Read { .. } | Change::Labels { .. } => {
                return Err(ProviderError::permanent("this change moves no message"));
            }
        };
        if graph_id(&id) {
            Ok(id)
        } else {
            Err(unsendable("folder"))
        }
    }
}
