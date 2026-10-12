//! `draft.push` and `draft.import`: a draft saved to Gmail's Drafts after the undo grace, and
//! a Gmail draft made into a local one that keeps its Gmail ids. Front-end operations only:
//! no MCP tool reaches them. Logs carry ids, never an address, a subject or a body.

use std::path::Path;

use super::drafts::{
    checked_id, draft_and_address, header_id, no_such_draft, row_of_draft, saved_id, unexpected,
};
use crate::auth::unix_now;
use crate::daemon::DaemonContext;
use crate::daemon::registry::Call;
use crate::ops::{
    Action, BodyFetchRequest, DraftId, DraftImportRequest, DraftImportResponse, DraftPushRequest,
    DraftPushResponse, OpError, UNDO_GRACE, checked_token, new_message_id,
};
use crate::store::Store;
use crate::store::drafts::{Attachment, Draft, DraftKind, DraftState, DraftWrite};
use crate::store::drafts_folder::ImportSource;
use crate::store::outbox::{NewAction, OutboxOutcome, OutboxWrite};
use crate::sync::manager::Reach;

fn being_sent() -> OpError {
    OpError::invalid_input("draft: it is being sent")
}

fn no_such_message() -> OpError {
    OpError::not_found("message: no such message")
}

fn checked(token: &str) -> Result<(), OpError> {
    checked_token(token).map_err(|e| OpError::invalid_input(format!("client_token: {}", e.message)))
}

/// Nothing a Gmail draft would hold: no recipient, subject, text or attachment.
fn is_empty(draft: &Draft) -> bool {
    draft.to.is_empty()
        && draft.cc.is_empty()
        && draft.bcc.is_empty()
        && draft.subject.is_empty()
        && draft.body.trim().is_empty()
        && draft.attachments.is_empty()
}

fn check_pushable(draft: &Draft) -> Result<(), OpError> {
    if draft.state == DraftState::Queued {
        return Err(being_sent());
    }
    if is_empty(draft) {
        return Err(OpError::invalid_input("draft: there is nothing to save"));
    }
    Ok(())
}

/// Why an enqueue wrote no row: the draft is being sent (its state changed in between), or
/// it is gone.
fn not_queued(db: &Path, id: DraftId) -> Result<OpError, OpError> {
    let draft = Store::open_reader(db)?.draft(id)?;
    Ok(match draft {
        Some(d) if d.state == DraftState::Queued => being_sent(),
        _ => no_such_draft(),
    })
}

/// The draft's account, checked as a send checks it, and the draft pushable.
async fn push_account(call: &Call, id: DraftId) -> Result<(i64, String), OpError> {
    let db = call.ctx.paths.db_file();
    let (draft, address) = call
        .ctx
        .blocking(move || draft_and_address(&db, id))
        .await?;
    let target = call.ctx.sync.action_target(&call.ctx, &address).await?;
    if target.account_id != draft.account_id {
        return Err(no_such_draft());
    }
    check_pushable(&draft)?;
    Ok((target.account_id, target.name))
}

pub(super) async fn push(
    call: Call,
    request: DraftPushRequest,
) -> Result<DraftPushResponse, OpError> {
    checked(&request.client_token)?;
    checked_id(request.draft)?;
    let id = request.draft;
    let (account_id, name) = push_account(&call, id).await?;
    let grace = i64::try_from(UNDO_GRACE.as_secs()).unwrap_or(10);
    let new = NewAction {
        account_id,
        action: Action::PushDraft { draft: id },
        threads: Vec::new(),
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
    let OutboxOutcome::Enqueued { rows, .. } = outcome else {
        return Err(unexpected());
    };
    let db = call.ctx.paths.db_file();
    let reader = db.clone();
    let Some(&row) = rows.first() else {
        return Err(call.ctx.blocking(move || not_queued(&reader, id)).await?);
    };
    call.ctx
        .blocking(move || row_of_draft(&db, row, id))
        .await?;
    call.ctx.sync.outbox_changed(&call.ctx).await;
    tracing::info!(account = %name, draft = id, row, "a draft push was queued");
    Ok(DraftPushResponse { row })
}

fn read_source(db: &Path, message: i64) -> Result<Option<ImportSource>, OpError> {
    Ok(Store::open_reader(db)?.import_source(message)?)
}

fn existing_import(db: &Path, account_id: i64, remote: &str) -> Result<Option<DraftId>, OpError> {
    Ok(Store::open_reader(db)?.draft_by_remote_message(account_id, remote)?)
}

/// The source of `message`: this account's, and a draft.
async fn import_source(
    ctx: &DaemonContext,
    message: i64,
    account_id: i64,
) -> Result<ImportSource, OpError> {
    let db = ctx.paths.db_file();
    let source = ctx
        .blocking(move || read_source(&db, message))
        .await?
        .ok_or_else(no_such_message)?;
    if source.account_id != account_id {
        return Err(OpError::invalid_input(
            "message: not a message of this account",
        ));
    }
    if !source.is_draft {
        return Err(OpError::invalid_input("message: not a draft"));
    }
    Ok(source)
}

/// The Gmail draft whose message is `remote`.
async fn gmail_draft(ctx: &DaemonContext, account: &str, remote: &str) -> Result<String, OpError> {
    let target = match ctx.sync.body_target(account).await? {
        Reach::Ready(target) => target,
        Reach::Unavailable(error) => return Err(error),
    };
    target
        .provider
        .draft_find(remote)
        .await?
        .ok_or_else(|| OpError::not_found("Gmail no longer has this draft"))
}

/// `source` with its body: fetched first when the store has none. A body still missing
/// after the fetch imports as empty text.
async fn with_body(
    ctx: &DaemonContext,
    account: &str,
    source: ImportSource,
) -> Result<ImportSource, OpError> {
    if source.body_text.is_some() {
        return Ok(source);
    }
    let request = BodyFetchRequest {
        account: account.to_string(),
        message_ids: vec![source.remote_id.clone()],
    };
    crate::sync::body_fetch::run(ctx, request).await?;
    let (db, message) = (ctx.paths.db_file(), source.message);
    let fetched = ctx
        .blocking(move || read_source(&db, message))
        .await?
        .ok_or_else(no_such_message)?;
    if fetched.body_text.is_none() {
        tracing::warn!(
            message,
            body = "missing",
            "a draft was imported without its body"
        );
    }
    Ok(fetched)
}

/// The local draft an import writes: the source's recipients, text, threading and parts, and
/// its own `Message-ID` when the MIME builder accepts it (`header_id` checks the inner part
/// with `valid_message_id_inner`), a fresh one otherwise.
fn import_write(source: ImportSource, address: &str) -> DraftWrite {
    let in_reply_to = source.in_reply_to.as_deref().and_then(header_id);
    let references: Vec<String> = source
        .references
        .iter()
        .filter_map(|id| header_id(id))
        .collect();
    let message_id = source
        .message_id
        .as_deref()
        .and_then(header_id)
        .unwrap_or_else(|| new_message_id(address));
    let attachments = source
        .attachments
        .into_iter()
        .map(|a| Attachment::Part {
            message: source.message,
            part: a.part_id,
            name: a.filename,
            mime: a.mime_type,
            size: a.size,
        })
        .collect();
    DraftWrite {
        id: None,
        account_id: source.account_id,
        kind: if in_reply_to.is_some() {
            DraftKind::Reply
        } else {
            DraftKind::New
        },
        thread_id: Some(source.thread_id),
        remote_thread_id: Some(source.remote_thread_id),
        in_reply_to,
        references: (!references.is_empty()).then(|| references.join(" ")),
        to: source.to.into_iter().map(Into::into).collect(),
        cc: source.cc.into_iter().map(Into::into).collect(),
        bcc: source.bcc.into_iter().map(Into::into).collect(),
        subject: source.subject,
        body: source.body_text.unwrap_or_default(),
        attachments,
        message_id,
    }
}

pub(super) async fn import(
    call: Call,
    request: DraftImportRequest,
) -> Result<DraftImportResponse, OpError> {
    checked(&request.client_token)?;
    if request.message <= 0 {
        return Err(no_such_message());
    }
    let ctx = &call.ctx;
    let target = ctx.sync.action_target(ctx, &request.account).await?;
    let source = import_source(ctx, request.message, target.account_id).await?;
    let (db, remote) = (ctx.paths.db_file(), source.remote_id.clone());
    let account_id = target.account_id;
    if let Some(draft) = ctx
        .blocking(move || existing_import(&db, account_id, &remote))
        .await?
    {
        return Ok(DraftImportResponse { draft });
    }
    let remote_draft_id = gmail_draft(ctx, &request.account, &source.remote_id).await?;
    let source = with_body(ctx, &request.account, source).await?;
    let remote_message_id = source.remote_id.clone();
    let write = import_write(source, &target.address);
    let outcome = ctx
        .sync
        .writer(ctx)
        .await?
        .outbox(OutboxWrite::DraftImport {
            write: Box::new(write),
            remote_draft_id,
            remote_message_id,
            client_token: request.client_token,
        })
        .await?;
    let draft = saved_id(outcome)?;
    tracing::info!(account = %target.name, draft, message = request.message, "a Gmail draft was imported");
    Ok(DraftImportResponse { draft })
}
