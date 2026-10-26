//! `snooze.set` and `snooze.clear`. Front-end operations only: no MCP tool reaches them. The
//! threads are checked against the store before anything is written (an archive's checks). A
//! snooze takes the threads with mail in the inbox, or snoozed already, and leaves the others
//! out; it is refused only when it has none to take. Errors never quote an id.

use std::path::Path;

use super::actions::check_in_store;
use crate::auth::unix_now;
use crate::daemon::registry::Call;
use crate::ops::{
    Action, NOT_IN_INBOX, OpError, SnoozeClearRequest, SnoozeClearResponse, SnoozeSetRequest,
    SnoozeSetResponse, UNDO_GRACE,
};
use crate::store::Store;
use crate::store::outbox::{OutboxOutcome, OutboxWrite};
use crate::store::snoozes::SnoozeNew;
use crate::sync::Wants;

fn unexpected() -> OpError {
    OpError::internal("the store answered a snooze write unexpectedly")
}

fn unsnoozable(db: &Path, account_id: i64, threads: &[i64]) -> Result<Vec<i64>, OpError> {
    Ok(Store::open_reader(db)?.unsnoozable(account_id, threads)?)
}

/// The threads a snooze takes, and how many it leaves out: a thread with no mail in the inbox
/// and no snooze to move is left out, and a request with none to take is refused.
async fn snoozable(
    call: &Call,
    account_id: i64,
    threads: Vec<i64>,
) -> Result<(Vec<i64>, usize), OpError> {
    let (db, ids) = (call.ctx.paths.db_file(), threads.clone());
    let refused = call
        .ctx
        .blocking(move || unsnoozable(&db, account_id, &ids))
        .await?;
    if refused.len() == threads.len() {
        return Err(OpError::invalid_input(NOT_IN_INBOX));
    }
    // `unsnoozable` answers in id order.
    let taken = threads
        .into_iter()
        .filter(|t| refused.binary_search(t).is_err())
        .collect();
    Ok((taken, refused.len()))
}

pub(super) async fn set(
    call: Call,
    request: SnoozeSetRequest,
) -> Result<SnoozeSetResponse, OpError> {
    let threads = request.checked(unix_now())?;
    let archive = Action::Archive {};
    let target = call
        .ctx
        .sync
        .action_target(&call.ctx, &request.account, Wants::Action(&archive))
        .await?;
    check_in_store(&call, target.account_id, threads.clone(), &archive).await?;
    let (threads, left_out) = snoozable(&call, target.account_id, threads).await?;
    let grace = if request.grace {
        i64::try_from(UNDO_GRACE.as_secs()).unwrap_or(10)
    } else {
        0
    };
    let new = SnoozeNew {
        account_id: target.account_id,
        threads,
        until: request.until,
        not_before: unix_now() + grace,
        client_token: request.client_token,
    };
    let writer = call.ctx.sync.writer(&call.ctx).await?;
    let OutboxOutcome::Snoozed { rows, snoozed } = writer.outbox(OutboxWrite::Snooze(new)).await?
    else {
        return Err(unexpected());
    };
    if snoozed == 0 {
        // A sync deleted every thread after the check: nothing was written.
        return Err(OpError::not_found("the threads are no longer in the store")
            .with_hint("the list will refresh; act on what it shows"));
    }
    tracing::info!(
        account = %target.name,
        rows = rows.len(),
        threads = snoozed,
        left_out,
        "a snooze was queued"
    );
    call.ctx.sync.outbox_changed(&call.ctx).await;
    Ok(SnoozeSetResponse { rows, snoozed })
}

pub(super) async fn clear(
    call: Call,
    request: SnoozeClearRequest,
) -> Result<SnoozeClearResponse, OpError> {
    let threads = request.checked()?;
    // Putting mail back needs no Archive folder: only that the threads are the account's.
    let read = Action::Read { on: false };
    let target = call
        .ctx
        .sync
        .action_target(&call.ctx, &request.account, Wants::Action(&read))
        .await?;
    check_in_store(&call, target.account_id, threads.clone(), &read).await?;
    let write = OutboxWrite::Unsnooze {
        account_id: target.account_id,
        threads,
    };
    let writer = call.ctx.sync.writer(&call.ctx).await?;
    let OutboxOutcome::Unsnoozed { returned } = writer.outbox(write).await? else {
        return Err(unexpected());
    };
    tracing::info!(account = %target.name, threads = returned, "snoozed threads were returned");
    call.ctx.sync.outbox_changed(&call.ctx).await;
    Ok(SnoozeClearResponse { returned })
}
