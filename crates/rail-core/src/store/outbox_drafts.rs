//! The draft rows of the outbox, in the writer's transaction: queueing a `send` or a push, a
//! push's completion, discard and the Gmail delete it leaves, and the import of a Gmail draft.
//! A `push_draft` and a `delete_draft` row share the `save_draft` kind column; the payload's
//! `kind` tells them apart.

use rusqlite::{OptionalExtension, Transaction, params};

use super::StoreError;
use super::drafts::{self, Attachment, DraftSaved, DraftWrite, ListedFile};
use super::outbox::{
    DraftDeleteOutcome, NewAction, OutboxOutcome, OutboxWrite, draft_state, json, set_draft_state,
};
use crate::ops::{Action, DraftId, OutboxId};

/// The draft's push row in `state`, if any.
fn push_row(tx: &Transaction, draft: DraftId, state: &str) -> Result<Option<OutboxId>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT id FROM outbox WHERE draft_id = ?1 AND state = ?2
               AND json_extract(payload, '$.kind') = 'push_draft' ORDER BY id LIMIT 1",
            params![draft, state],
            |r| r.get(0),
        )
        .optional()?)
}

/// A draft row for one of the account's drafts; `None` (nothing written) when the account
/// holds no such draft, for a `send` of a draft that is not `editing` (already queued), or a
/// push of a draft being sent. A push while one of the draft's pushes still waits answers
/// that row (the worker pushes the newest text once) and takes it over: its token undoes it
/// and its grace starts again. A `send` row queues the draft.
pub(super) fn enqueue_draft(
    tx: &Transaction,
    new: &NewAction,
    draft: DraftId,
) -> Result<Option<(OutboxId, usize)>, StoreError> {
    let Some(state) = draft_state(tx, draft, Some(new.account_id))? else {
        return Ok(None);
    };
    match new.action {
        Action::Send { .. } if state != "editing" => return Ok(None),
        Action::PushDraft { .. } if state == "queued" => return Ok(None),
        Action::PushDraft { .. } => {
            if let Some(row) = push_row(tx, draft, "pending")? {
                fold_push(tx, row, new)?;
                return Ok(Some((row, 1)));
            }
        }
        _ => {}
    }
    tx.execute(
        "INSERT INTO outbox (account_id, kind, payload, state, client_token, not_before, thread_ids,
           reversal, draft_id)
         VALUES (?1, ?2, ?3, 'pending', ?4, ?5, '[]', '[]', ?6)",
        params![
            new.account_id,
            new.action.kind_column(),
            json(&new.action),
            new.client_token,
            new.not_before,
            draft
        ],
    )?;
    let id = tx.last_insert_rowid();
    if matches!(new.action, Action::Send { .. }) {
        set_draft_state(tx, draft, "queued")?;
    }
    Ok(Some((id, 1)))
}

/// The newest push owns the pending row: its token and its `not_before`. A replayed older
/// push (its token gone from the outbox, its `not_before` earlier) is answered the row
/// without taking it back.
fn fold_push(tx: &Transaction, row: OutboxId, new: &NewAction) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE outbox SET client_token = ?2, not_before = ?3
         WHERE id = ?1 AND state = 'pending' AND not_before <= ?3",
        params![row, new.client_token, new.not_before],
    )?;
    Ok(())
}

/// A pending Gmail delete of `remote_draft_id`, due `now`. Keyed by no draft (`draft_id`
/// NULL: the local draft is gone or about to go); its payload still names `draft`.
pub(super) fn insert_delete_draft(
    tx: &Transaction,
    account_id: i64,
    draft: DraftId,
    remote_draft_id: &str,
    token: &str,
    now: i64,
) -> Result<OutboxId, StoreError> {
    let action = Action::DeleteDraft {
        draft,
        remote_draft_id: remote_draft_id.to_string(),
    };
    tx.execute(
        "INSERT INTO outbox (account_id, kind, payload, state, client_token, not_before, thread_ids,
           reversal, draft_id)
         VALUES (?1, ?2, ?3, 'pending', ?4, ?5, '[]', '[]', NULL)",
        params![account_id, action.kind_column(), json(&action), token, now],
    )?;
    Ok(tx.last_insert_rowid())
}

/// The draft's account and Gmail draft id, when it exists.
pub(super) fn draft_remote(
    tx: &Transaction,
    draft: DraftId,
) -> Result<Option<(i64, Option<String>)>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT account_id, remote_draft_id FROM drafts WHERE id = ?1",
            [draft],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// `draft.discard` in the writer's transaction. Refused while the draft is being sent or a
/// push of it is in flight (its Gmail id may be about to change); otherwise the draft and its
/// rows go, and a draft Gmail holds leaves a Gmail delete. A missing draft is gone already.
pub(super) fn discard_in(
    tx: &Transaction,
    id: DraftId,
    now: i64,
) -> Result<DraftDeleteOutcome, StoreError> {
    if drafts::being_sent(tx, id)? {
        return Ok(DraftDeleteOutcome::BeingSent);
    }
    if push_row(tx, id, "inflight")?.is_some() {
        return Ok(DraftDeleteOutcome::BeingPushed);
    }
    let Some((account_id, remote)) = draft_remote(tx, id)? else {
        return Ok(DraftDeleteOutcome::Deleted);
    };
    tx.execute("DELETE FROM outbox WHERE draft_id = ?1", [id])?;
    tx.execute("DELETE FROM drafts WHERE id = ?1", [id])?;
    if let Some(remote) = remote {
        let token = format!("discard-{id}-{now}");
        insert_delete_draft(tx, account_id, id, &remote, &token, now)?;
    }
    Ok(DraftDeleteOutcome::Deleted)
}

/// A push landed: the draft (when it still exists) keeps Gmail's ids, and the row goes.
pub(super) fn complete_push(
    tx: &Transaction,
    id: OutboxId,
    draft: DraftId,
    remote: (&str, &str),
    listed: &[ListedFile],
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE drafts SET remote_draft_id = ?2, remote_message_id = ?3 WHERE id = ?1",
        params![draft, remote.0, remote.1],
    )?;
    record_listed(tx, draft, listed)?;
    tx.execute("DELETE FROM outbox WHERE id = ?1", [id])?;
    Ok(())
}

/// Record on the draft's files the sizes the provider lists for them: each on the file entry
/// with its path and size in bytes, so an entry made for a file replaced since keeps none. A
/// list that does not parse is left as it is (`draft_attachments_readable` refuses it).
fn record_listed(
    tx: &Transaction,
    draft: DraftId,
    listed: &[ListedFile],
) -> Result<(), StoreError> {
    if listed.is_empty() {
        return Ok(());
    }
    let text: Option<String> = tx
        .query_row(
            "SELECT attachments_json FROM drafts WHERE id = ?1",
            [draft],
            |r| r.get(0),
        )
        .optional()?;
    let Some(mut attachments) = text.and_then(|t| serde_json::from_str::<Vec<Attachment>>(&t).ok())
    else {
        return Ok(());
    };
    let mut changed = false;
    for attachment in &mut attachments {
        let Attachment::File {
            path,
            size,
            listed: recorded,
            ..
        } = attachment
        else {
            continue;
        };
        let found = listed.iter().find(|l| l.path == *path && l.size == *size);
        if let Some(l) = found.filter(|l| *recorded != Some(l.listed)) {
            *recorded = Some(l.listed);
            changed = true;
        }
    }
    if changed {
        tx.execute(
            "UPDATE drafts SET attachments_json = ?2 WHERE id = ?1",
            params![draft, drafts::json(&attachments)],
        )?;
    }
    Ok(())
}

/// The import of a Gmail draft: the account's draft of that Gmail message, or of that Gmail
/// draft (edited elsewhere since Rail took it: Rail's version wins), when one exists; else a
/// new draft (by `drafts::save_in`, so a replayed token answers the draft it made) that keeps
/// Gmail's ids. An import only creates: a write naming a draft is `Missing`, and a draft that
/// already has a Gmail identity (a replayed token of an earlier save) keeps it.
pub(super) fn import_in(
    tx: &Transaction,
    write: &DraftWrite,
    remote_draft_id: &str,
    remote_message_id: &str,
    token: &str,
    now: i64,
) -> Result<DraftSaved, StoreError> {
    if write.id.is_some() {
        return Ok(DraftSaved::Missing);
    }
    let known: Option<DraftId> = tx
        .query_row(
            "SELECT id FROM drafts WHERE account_id = ?1
               AND (remote_message_id = ?2 OR remote_draft_id = ?3)
             ORDER BY id LIMIT 1",
            params![write.account_id, remote_message_id, remote_draft_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = known {
        return Ok(DraftSaved::Saved(id));
    }
    // Tokens are unique across accounts; a token another call used binds nothing here.
    let used: bool = tx.query_row(
        "SELECT EXISTS (SELECT 1 FROM drafts WHERE client_token = ?1)",
        [token],
        |r| r.get(0),
    )?;
    if used {
        return Ok(DraftSaved::TokenUsed);
    }
    let saved = drafts::save_in(tx, write, token, now)?;
    if let DraftSaved::Saved(id) = saved {
        tx.execute(
            "UPDATE drafts SET remote_draft_id = ?2, remote_message_id = ?3
             WHERE id = ?1 AND remote_message_id IS NULL",
            params![id, remote_draft_id, remote_message_id],
        )?;
    }
    Ok(saved)
}

/// The writes that change a draft row, in the caller's transaction (`outbox_write` passes only
/// those).
pub(super) fn draft_write(
    tx: &Transaction,
    write: &OutboxWrite,
    now: i64,
) -> Result<OutboxOutcome, StoreError> {
    Ok(match write {
        OutboxWrite::DraftSendAttempted {
            draft,
            remote,
            listed,
        } => {
            tx.execute(
                "UPDATE drafts SET send_attempted_at = ?2
                 WHERE id = ?1 AND send_attempted_at IS NULL",
                params![draft, now],
            )?;
            if let Some(ids) = remote {
                tx.execute(
                    "UPDATE drafts SET remote_draft_id = ?2, remote_message_id = ?3 WHERE id = ?1",
                    params![draft, ids.draft_id, ids.message_id],
                )?;
            }
            record_listed(tx, *draft, listed)?;
            OutboxOutcome::Done
        }
        OutboxWrite::DraftSave {
            write,
            client_token,
        } => OutboxOutcome::DraftSaved(drafts::save_in(tx, write, client_token, now)?),
        OutboxWrite::DraftDelete(id) => OutboxOutcome::DraftDeleted(discard_in(tx, *id, now)?),
        OutboxWrite::CompletePush {
            id,
            draft,
            remote_draft_id,
            remote_message_id,
            listed,
        } => {
            let remote = (remote_draft_id.as_str(), remote_message_id.as_str());
            complete_push(tx, *id, *draft, remote, listed)?;
            OutboxOutcome::Done
        }
        OutboxWrite::DraftImport {
            write,
            remote_draft_id,
            remote_message_id,
            client_token,
        } => OutboxOutcome::DraftSaved(import_in(
            tx,
            write,
            remote_draft_id,
            remote_message_id,
            client_token,
            now,
        )?),
        _ => OutboxOutcome::Done,
    })
}
