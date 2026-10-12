//! `draft.save`, `draft.send` and `draft.discard`. Front-end operations only: no MCP tool
//! reaches them. Reads open a reader; every write goes through the writer's outbox path, so
//! the store keeps one writer. Logs carry ids and counts, never an address, a subject or a
//! file name (a file name appears only in the refusal the user reads).

use std::path::Path;

use crate::auth::unix_now;
use crate::daemon::registry::Call;
use crate::ops::drafts::{AttachmentSummary, CheckedRecipients, DraftAttachment};
use crate::ops::{
    Action, DraftDiscardRequest, DraftDiscardResponse, DraftId, DraftSaveRequest,
    DraftSaveResponse, DraftSendRequest, DraftSendResponse, MAX_ATTACHMENTS,
    MAX_TOTAL_ATTACHMENT_BYTES, OpError, ReplyTo, UNDO_GRACE, checked_token, new_message_id,
};
use crate::provider::gmail::mime::valid_message_id_inner;
use crate::store::Store;
use crate::store::drafts::{Attachment, Draft, DraftKind, DraftSaved, DraftState, DraftWrite};
use crate::store::outbox::{NewAction, OutboxOutcome, OutboxWrite};

fn unexpected() -> OpError {
    OpError::internal("the store answered a draft write unexpectedly")
}

fn no_such_draft() -> OpError {
    OpError::not_found("draft: no such draft")
}

fn being_sent() -> OpError {
    OpError::invalid_input("draft: this draft is being sent; undo first")
}

fn checked_id(draft: DraftId) -> Result<(), OpError> {
    if draft <= 0 {
        return Err(OpError::invalid_input(
            "draft: a draft id is a positive number",
        ));
    }
    Ok(())
}

/// One file to attach, checked now: a regular file this user can open. The name is the
/// file's own, the type guessed from its extension.
fn file_attachment(path: String) -> Result<Attachment, OpError> {
    let name = Path::new(&path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let refused = |why: String| OpError::invalid_input(format!("attachments: {name:?} {why}"));
    let meta = std::fs::metadata(&path).map_err(|e| refused(format!("cannot be read: {e}")))?;
    if !meta.is_file() {
        return Err(refused("is not a regular file".into()));
    }
    std::fs::File::open(&path).map_err(|e| refused(format!("cannot be read: {e}")))?;
    Ok(Attachment::File {
        mime: mime_of(&name).to_string(),
        name,
        path,
        size: i64::try_from(meta.len()).unwrap_or(i64::MAX),
    })
}

/// Every file of `paths`, checked in order. Runs on the blocking pool.
fn file_attachments(paths: Vec<String>) -> Result<Vec<Attachment>, OpError> {
    paths.into_iter().map(file_attachment).collect()
}

/// At most `MAX_ATTACHMENTS`, together at most Gmail's 25 MiB.
fn check_total<'a>(attachments: impl Iterator<Item = &'a Attachment>) -> Result<(), OpError> {
    let (mut count, mut total) = (0usize, 0u64);
    for attachment in attachments {
        count += 1;
        total = total.saturating_add(u64::try_from(attachment.size()).unwrap_or(0));
    }
    if count > MAX_ATTACHMENTS {
        return Err(OpError::invalid_input(format!(
            "attachments: at most {MAX_ATTACHMENTS} attachments"
        )));
    }
    if total > MAX_TOTAL_ATTACHMENT_BYTES {
        return Err(OpError::invalid_input(
            "attachments: together larger than Gmail's 25 MiB limit",
        ));
    }
    Ok(())
}

/// A MIME type from the extension; `application/octet-stream` otherwise.
fn mime_of(name: &str) -> &'static str {
    let extension = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase());
    match extension.as_deref() {
        Some("pdf") => "application/pdf",
        Some("txt" | "md" | "log") => "text/plain",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("zip") => "application/zip",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        _ => "application/octet-stream",
    }
}

/// The threading headers and forwarded parts a reply or forward takes from the store.
#[derive(Default)]
struct Threading {
    thread_id: Option<i64>,
    remote_thread_id: Option<String>,
    in_reply_to: Option<String>,
    references: Option<String>,
    parts: Vec<Attachment>,
}

/// A stored id as a threading header carries it: `<inner>`. Gmail's metadata keeps ids bare
/// and either form is accepted. `None` when the inner part fails the MIME builder's rule: a bad
/// id in received mail is left out of the headers rather than blocking the reply.
fn header_id(id: &str) -> Option<String> {
    let id = id.trim();
    let inner = id.strip_prefix('<').unwrap_or(id);
    let inner = inner.strip_suffix('>').unwrap_or(inner);
    valid_message_id_inner(inner).then(|| format!("<{inner}>"))
}

fn threading_for(
    store: &Store,
    kind: DraftKind,
    reply_to: Option<&ReplyTo>,
    account_id: i64,
) -> Result<Threading, OpError> {
    let Some(reply) = reply_to.filter(|_| kind != DraftKind::New) else {
        return Ok(Threading::default());
    };
    let source = store
        .reply_source(reply.message)?
        .filter(|s| s.account_id == account_id)
        .ok_or_else(|| {
            OpError::not_found("reply_to: the message being answered is no longer in the store")
        })?;
    let in_reply_to = source.message_id.as_deref().and_then(header_id);
    // Space-joined: the worker splits `references` on whitespace.
    let mut references: Vec<String> = source
        .references
        .iter()
        .filter_map(|id| header_id(id))
        .collect();
    references.extend(in_reply_to.clone());
    let parts = match kind {
        DraftKind::Forward => source
            .attachments
            .iter()
            .map(|a| Attachment::Part {
                message: reply.message,
                part: a.part_id.clone(),
                name: a.filename.clone(),
                mime: a.mime_type.clone(),
                size: a.size,
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(Threading {
        thread_id: Some(source.thread_id),
        remote_thread_id: Some(source.remote_thread_id),
        in_reply_to,
        references: (!references.is_empty()).then(|| references.join(" ")),
        parts,
    })
}

/// What a save writes, checked against the store.
struct SaveInput {
    account_id: i64,
    address: String,
    request: DraftSaveRequest,
    recipients: CheckedRecipients,
    files: Vec<Attachment>,
}

/// The draft an update names: the account's, and not being sent.
fn editable(
    store: &Store,
    draft: Option<DraftId>,
    account_id: i64,
) -> Result<Option<Draft>, OpError> {
    let Some(id) = draft else {
        return Ok(None);
    };
    let draft = store
        .draft(id)?
        .filter(|d| d.account_id == account_id)
        .ok_or_else(no_such_draft)?;
    if draft.state == DraftState::Queued {
        return Err(being_sent());
    }
    Ok(Some(draft))
}

/// The draft to write: the existing one (the account's, not being sent) updated, or a new
/// one with a fresh `Message-ID`. An update keeps the forwarded parts it has; the request's
/// files replace its files.
fn compose_write(db: &Path, input: SaveInput) -> Result<DraftWrite, OpError> {
    let store = Store::open_reader(db)?;
    let SaveInput {
        account_id,
        address,
        request,
        recipients,
        files,
    } = input;
    let existing = editable(&store, request.draft, account_id)?;
    let threading = threading_for(&store, request.kind, request.reply_to.as_ref(), account_id)?;
    let mut attachments = match &existing {
        Some(d) => d
            .attachments
            .iter()
            .filter(|a| matches!(a, Attachment::Part { .. }))
            .cloned()
            .collect(),
        None => threading.parts,
    };
    attachments.extend(files);
    check_total(attachments.iter())?;
    Ok(DraftWrite {
        id: existing.as_ref().map(|d| d.id),
        account_id,
        kind: request.kind,
        thread_id: threading.thread_id,
        remote_thread_id: threading.remote_thread_id,
        in_reply_to: threading.in_reply_to,
        references: threading.references,
        to: recipients.to,
        cc: recipients.cc,
        bcc: recipients.bcc,
        subject: request.subject,
        body: request.body,
        attachments,
        message_id: existing.map_or_else(|| new_message_id(&address), |d| d.message_id),
    })
}

fn summary(attachment: &Attachment) -> AttachmentSummary {
    AttachmentSummary {
        name: attachment.name().to_string(),
        mime: attachment.mime().to_string(),
        size: attachment.size(),
        path: match attachment {
            Attachment::File { path, .. } => Some(path.clone()),
            Attachment::Part { .. } => None,
        },
    }
}

/// The saved draft as the store holds it now.
fn saved(db: &Path, id: DraftId) -> Result<DraftSaveResponse, OpError> {
    let draft = Store::open_reader(db)?
        .draft(id)?
        .ok_or_else(no_such_draft)?;
    Ok(DraftSaveResponse {
        draft: id,
        message_id: draft.message_id,
        attachments: draft.attachments.iter().map(summary).collect(),
    })
}

fn saved_id(outcome: OutboxOutcome) -> Result<DraftId, OpError> {
    match outcome {
        OutboxOutcome::DraftSaved(DraftSaved::Saved(id)) => Ok(id),
        OutboxOutcome::DraftSaved(DraftSaved::Missing) => Err(no_such_draft()),
        OutboxOutcome::DraftSaved(DraftSaved::Queued) => Err(being_sent()),
        _ => Err(unexpected()),
    }
}

pub(super) async fn save(
    call: Call,
    request: DraftSaveRequest,
) -> Result<DraftSaveResponse, OpError> {
    let recipients = request.checked()?;
    let target = call
        .ctx
        .sync
        .action_target(&call.ctx, &request.account)
        .await?;
    let paths: Vec<String> = request
        .attachments
        .iter()
        .map(|DraftAttachment::File { path }| path.clone())
        .collect();
    let files = call.ctx.blocking(move || file_attachments(paths)).await?;
    let (db, client_token) = (call.ctx.paths.db_file(), request.client_token.clone());
    let input = SaveInput {
        account_id: target.account_id,
        address: target.address.clone(),
        request,
        recipients,
        files,
    };
    let reader = db.clone();
    let write = call
        .ctx
        .blocking(move || compose_write(&reader, input))
        .await?;
    let outcome = call
        .ctx
        .sync
        .writer(&call.ctx)
        .await?
        .outbox(OutboxWrite::DraftSave {
            write: Box::new(write),
            client_token,
        })
        .await?;
    let id = saved_id(outcome)?;
    let response = call.ctx.blocking(move || saved(&db, id)).await?;
    let attachments = response.attachments.len();
    tracing::info!(account = %target.name, draft = id, attachments, "a draft was saved");
    Ok(response)
}

/// The draft and its account's address.
fn draft_and_address(db: &Path, id: DraftId) -> Result<(Draft, String), OpError> {
    let store = Store::open_reader(db)?;
    let draft = store.draft(id)?.ok_or_else(no_such_draft)?;
    if !store.draft_attachments_readable(id)? {
        return Err(OpError::invalid_input(
            "attachments: the stored list cannot be read; attach the files again",
        ));
    }
    let address = store
        .list_accounts()?
        .into_iter()
        .find(|a| a.id == draft.account_id)
        .map(|a| a.address)
        .ok_or_else(no_such_draft)?;
    Ok((draft, address))
}

/// Every file still there and readable, and the total still under the cap.
fn check_sendable(draft: &Draft) -> Result<(), OpError> {
    if draft.to.is_empty() && draft.cc.is_empty() && draft.bcc.is_empty() {
        return Err(OpError::invalid_input("to: add at least one recipient"));
    }
    let paths = draft
        .attachments
        .iter()
        .filter_map(|a| match a {
            Attachment::File { path, .. } => Some(path.clone()),
            Attachment::Part { .. } => None,
        })
        .collect();
    let files = file_attachments(paths)?;
    let parts = draft
        .attachments
        .iter()
        .filter(|a| matches!(a, Attachment::Part { .. }));
    check_total(files.iter().chain(parts))
}

/// Why an enqueue wrote no row: the draft is being sent, or it is gone.
fn not_queued(db: &Path, id: DraftId) -> Result<OpError, OpError> {
    let draft = Store::open_reader(db)?.draft(id)?;
    Ok(match draft {
        Some(d) if d.state == DraftState::Queued => {
            OpError::invalid_input("draft: this draft is already being sent")
        }
        _ => no_such_draft(),
    })
}

/// A token already in the outbox answers its row: refuse it unless that row is this draft's.
fn row_of_draft(db: &Path, row: i64, id: DraftId) -> Result<(), OpError> {
    let owner = Store::open_reader(db)?
        .outbox_row(row)?
        .and_then(|r| r.draft_id);
    if owner != Some(id) {
        return Err(OpError::invalid_input(
            "client_token: this token was used for another action",
        ));
    }
    Ok(())
}

pub(super) async fn send(
    call: Call,
    request: DraftSendRequest,
) -> Result<DraftSendResponse, OpError> {
    checked_token(&request.client_token)
        .map_err(|e| OpError::invalid_input(format!("client_token: {}", e.message)))?;
    checked_id(request.draft)?;
    let (db, id) = (call.ctx.paths.db_file(), request.draft);
    let reader = db.clone();
    let (draft, address) = call
        .ctx
        .blocking(move || draft_and_address(&reader, id))
        .await?;
    let target = call.ctx.sync.action_target(&call.ctx, &address).await?;
    if target.account_id != draft.account_id {
        return Err(no_such_draft());
    }
    let account_id = draft.account_id;
    call.ctx.blocking(move || check_sendable(&draft)).await?;
    let grace = i64::try_from(UNDO_GRACE.as_secs()).unwrap_or(10);
    let new = NewAction {
        account_id,
        action: Action::Send { draft: id },
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
    let reader = db.clone();
    let Some(&row) = rows.first() else {
        return Err(call.ctx.blocking(move || not_queued(&reader, id)).await?);
    };
    call.ctx
        .blocking(move || row_of_draft(&db, row, id))
        .await?;
    tracing::info!(account = %target.name, draft = id, row, "a send was queued");
    call.ctx.sync.outbox_changed(&call.ctx).await;
    Ok(DraftSendResponse { row })
}

pub(super) async fn discard(
    call: Call,
    request: DraftDiscardRequest,
) -> Result<DraftDiscardResponse, OpError> {
    checked_id(request.draft)?;
    let outcome = call
        .ctx
        .sync
        .writer(&call.ctx)
        .await?
        .outbox(OutboxWrite::DraftDelete(request.draft))
        .await?;
    match outcome {
        OutboxOutcome::DraftDeleted(true) => {}
        OutboxOutcome::DraftDeleted(false) => return Err(being_sent()),
        _ => return Err(unexpected()),
    }
    tracing::info!(draft = request.draft, "a draft was discarded");
    call.ctx.sync.outbox_changed(&call.ctx).await;
    Ok(DraftDiscardResponse {})
}
