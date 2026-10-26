//! `action.apply`, `action.undo` and `action.retry`. Front-end operations only: no MCP tool
//! reaches them. Ids are checked against the store before anything is written; errors never
//! quote an id.

use std::path::Path;

use crate::auth::unix_now;
use crate::config::FolderModel;
use crate::daemon::registry::Call;
use crate::error::ErrorClass;
use crate::ops::{
    Action, ActionApplyRequest, ActionApplyResponse, ActionRetryRequest, ActionRetryResponse,
    ActionUndoRequest, ActionUndoResponse, ErrorCode, M365_NO_ARCHIVE, MAX_UNDO_ROWS, OpError,
    UNDO_GRACE, checked_token,
};
use crate::provider::FolderKind;
use crate::store::Store;
use crate::store::outbox::{NewAction, OutboxOutcome, OutboxWrite};
use crate::sync::Wants;

fn unexpected() -> OpError {
    OpError::internal("the store answered an outbox write unexpectedly")
}

/// What the store says of an action on these threads.
struct InStore {
    held: usize,
    labelled: bool,
    movable: bool,
    archivable: bool,
}

fn read_in_store(
    db: &Path,
    account_id: i64,
    threads: &[i64],
    action: &Action,
) -> Result<InStore, OpError> {
    let store = Store::open_reader(db)?;
    let model = store.folder_model(account_id)?;
    let labelled = match action {
        Action::Labels { .. } => store.user_labels(account_id, &action.label_ids())?,
        _ => true,
    };
    let movable = match action {
        Action::Move { to } => store.move_target(account_id, *to, model)?,
        _ => true,
    };
    let archivable = !matches!(action, Action::Archive {})
        || model == FolderModel::Labels
        || store
            .folder_of_kind(account_id, FolderKind::Archive)?
            .is_some();
    Ok(InStore {
        held: store.thread_remotes(account_id, threads)?.len(),
        labelled,
        movable,
        archivable,
    })
}

/// Every thread is the account's, every label one of its labels, a move's folder one of its
/// move targets (`FolderModel::move_kinds`), and a one-folder mailbox that archives has an
/// Archive folder.
pub(super) async fn check_in_store(
    call: &Call,
    account_id: i64,
    threads: Vec<i64>,
    action: &Action,
) -> Result<(), OpError> {
    let (db, wanted, action) = (call.ctx.paths.db_file(), threads.len(), action.clone());
    let found = call
        .ctx
        .blocking(move || read_in_store(&db, account_id, &threads, &action))
        .await?;
    if found.held != wanted {
        return Err(OpError::invalid_input(
            "a thread is not one of this account's threads in the store",
        ));
    }
    if !found.labelled {
        return Err(
            OpError::invalid_input("a label is not one of this account's labels")
                .with_hint("the label may have been deleted in Gmail; open the picker again"),
        );
    }
    if !found.movable {
        return Err(
            OpError::invalid_input("a folder is not one of this account's move targets")
                .with_hint("the folder may have been deleted; open the picker again"),
        );
    }
    if !found.archivable {
        return Err(OpError::new(
            ErrorCode::Unsupported,
            ErrorClass::Permanent,
            M365_NO_ARCHIVE,
        ));
    }
    Ok(())
}

pub(super) async fn apply(
    call: Call,
    request: ActionApplyRequest,
) -> Result<ActionApplyResponse, OpError> {
    let threads = request.checked()?;
    let target = call
        .ctx
        .sync
        .action_target(&call.ctx, &request.account, Wants::Action(&request.action))
        .await?;
    check_in_store(&call, target.account_id, threads.clone(), &request.action).await?;
    let grace = if request.grace {
        i64::try_from(UNDO_GRACE.as_secs()).unwrap_or(10)
    } else {
        0
    };
    let new = NewAction {
        account_id: target.account_id,
        action: request.action,
        threads,
        not_before: unix_now() + grace,
        client_token: request.client_token,
    };
    let outcome = call
        .ctx
        .sync
        .writer(&call.ctx)
        .await?
        .outbox(OutboxWrite::Enqueue(new))
        .await?;
    let OutboxOutcome::Enqueued { rows, applied } = outcome else {
        return Err(unexpected());
    };
    if rows.is_empty() {
        // A sync deleted every thread after the check: nothing was written.
        return Err(OpError::not_found("the threads are no longer in the store")
            .with_hint("the list will refresh; act on what it shows"));
    }
    tracing::info!(account = %target.name, rows = rows.len(), threads = applied, "an action was queued");
    call.ctx.sync.outbox_changed(&call.ctx).await;
    Ok(ActionApplyResponse { rows, applied })
}

pub(super) async fn undo(
    call: Call,
    request: ActionUndoRequest,
) -> Result<ActionUndoResponse, OpError> {
    if request.rows.is_empty()
        || request.rows.len() > MAX_UNDO_ROWS
        || request.rows.iter().any(|r| r.row <= 0)
    {
        return Err(OpError::invalid_input(format!(
            "an undo takes 1 to {MAX_UNDO_ROWS} positive row ids"
        )));
    }
    for row in &request.rows {
        checked_token(&row.client_token)?;
    }
    let outcome = call
        .ctx
        .sync
        .writer(&call.ctx)
        .await?
        .outbox(OutboxWrite::Undo { rows: request.rows })
        .await?;
    let OutboxOutcome::Undone { undone, too_late } = outcome else {
        return Err(unexpected());
    };
    call.ctx.sync.outbox_changed(&call.ctx).await;
    Ok(ActionUndoResponse { undone, too_late })
}

pub(super) async fn retry(
    call: Call,
    request: ActionRetryRequest,
) -> Result<ActionRetryResponse, OpError> {
    if request.row <= 0 {
        return Err(OpError::invalid_input("a row id is a positive number"));
    }
    checked_token(&request.client_token)?;
    let outcome = call
        .ctx
        .sync
        .writer(&call.ctx)
        .await?
        .outbox(OutboxWrite::Retry {
            id: request.row,
            client_token: request.client_token,
        })
        .await?;
    let OutboxOutcome::Retried(retried) = outcome else {
        return Err(unexpected());
    };
    call.ctx.sync.outbox_changed(&call.ctx).await;
    Ok(ActionRetryResponse { retried })
}
