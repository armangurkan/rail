//! `draft.save`, `draft.send` and `draft.discard` (which removes an imported draft's files).
//! Front-end operations only: no MCP tool
//! reaches them. Reads open a reader; every write goes through the writer's outbox path, so
//! the store keeps one writer. Logs carry ids and counts, never an address, a subject or a
//! file name (a file name appears only in the refusal the user reads).

use std::path::{Path, PathBuf};

use super::draft_files::{import_dir, remove_dirs};

use crate::auth::unix_now;
use crate::config::AccountKind;
use crate::daemon::registry::Call;
use crate::error::ErrorClass;
use crate::ops::drafts::{AttachmentSummary, CheckedRecipients, DraftAttachment};
use crate::ops::{
    Action, DraftDiscardRequest, DraftDiscardResponse, DraftId, DraftSaveRequest,
    DraftSaveResponse, DraftSendRequest, DraftSendResponse, ErrorCode, MAX_ATTACHMENTS,
    MAX_MESSAGE_BYTES, OpError, ReplyTo, UNDO_GRACE, checked_token, estimated_raw_size,
    new_message_id, too_large_text,
};
use crate::paths::Paths;
use crate::provider::gmail::mime::valid_message_id_inner;
use crate::store::Store;
use crate::store::drafts::{Attachment, Draft, DraftKind, DraftSaved, DraftState, DraftWrite};
use crate::store::outbox::{DraftDeleteOutcome, NewAction, OutboxOutcome, OutboxWrite};
use crate::sync::Wants;

pub(super) fn unexpected() -> OpError {
    OpError::internal("the store answered a draft write unexpectedly")
}

pub(super) fn no_such_draft() -> OpError {
    OpError::not_found("draft: no such draft")
}

pub(super) fn being_sent() -> OpError {
    OpError::invalid_input("draft: this draft is being sent; undo first")
}

/// A push of the draft is in flight: its provider id may be about to change, so a discard now
/// could leave the provider's copy behind. It clears up once the push lands.
fn being_pushed(kind: AccountKind) -> OpError {
    OpError::new(
        ErrorCode::InvalidInput,
        ErrorClass::Transient,
        format!(
            "draft: saving to {}, try again in a moment",
            kind.provider_name()
        ),
    )
}

/// The request's client token, refused under its field's name.
pub(super) fn checked_client_token(token: &str) -> Result<(), OpError> {
    checked_token(token).map_err(|e| OpError::invalid_input(format!("client_token: {}", e.message)))
}

pub(super) fn token_used() -> OpError {
    OpError::invalid_input("client_token: this token was used for another action")
}

pub(super) fn checked_id(draft: DraftId) -> Result<(), OpError> {
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

/// At most `MAX_ATTACHMENTS`, and the message with `body_bytes` of text at most the 25 MiB the
/// MIME builder estimates (Gmail's limit, kept for every provider; attachments grow by a third
/// once encoded). The refusal names `provider`.
fn check_total<'a>(
    body_bytes: usize,
    attachments: impl Iterator<Item = &'a Attachment>,
    provider: &str,
) -> Result<(), OpError> {
    let sizes: Vec<usize> = attachments
        .map(|a| usize::try_from(a.size()).unwrap_or(0))
        .collect();
    if sizes.len() > MAX_ATTACHMENTS {
        return Err(OpError::invalid_input(format!(
            "attachments: at most {MAX_ATTACHMENTS} attachments"
        )));
    }
    let estimated = estimated_raw_size(body_bytes, &sizes);
    if estimated > MAX_MESSAGE_BYTES {
        return Err(OpError::invalid_input(format!(
            "attachments: {}",
            too_large_text(estimated, provider)
        )));
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
    /// The provider id of the message answered.
    source_remote_id: Option<String>,
    in_reply_to: Option<String>,
    references: Option<String>,
    parts: Vec<Attachment>,
}

/// A stored id as a threading header carries it: `<inner>`. Gmail's metadata keeps ids bare
/// and either form is accepted. `None` when the inner part fails the MIME builder's rule: a bad
/// id in received mail is left out of the headers rather than blocking the reply.
pub(super) fn header_id(id: &str) -> Option<String> {
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
        source_remote_id: Some(source.remote_id),
        in_reply_to,
        references: (!references.is_empty()).then(|| references.join(" ")),
        parts,
    })
}

/// A stored draft's threading as it is (its forwarded parts are kept by `kept_parts`).
fn kept_threading(d: &Draft) -> Threading {
    Threading {
        thread_id: d.thread_id,
        remote_thread_id: d.remote_thread_id.clone(),
        source_remote_id: d.source_remote_id.clone(),
        in_reply_to: d.in_reply_to.clone(),
        references: d.references.clone(),
        parts: Vec::new(),
    }
}

/// What a save writes, checked against the store.
struct SaveInput {
    account_id: i64,
    address: String,
    request: DraftSaveRequest,
    recipients: CheckedRecipients,
    files: Vec<Attachment>,
    /// The account's provider, named by the size refusal.
    provider: &'static str,
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

fn not_this_drafts() -> OpError {
    OpError::invalid_input("attachments: not an attachment of this draft")
}

/// Whether `stored` is the part `message`/`part` names.
fn is_part(stored: &Attachment, message: i64, part: &str) -> bool {
    matches!(stored, Attachment::Part { message: m, part: p, .. } if *m == message && p == part)
}

/// The forwarded parts a save keeps: on an update, the stored parts the request lists (one
/// the draft never had is refused); on a create, none may be listed and a forward takes its
/// parts from the store.
fn kept_parts(
    existing: Option<&Draft>,
    listed: &[DraftAttachment],
    derived: Vec<Attachment>,
) -> Result<Vec<Attachment>, OpError> {
    let listed: Vec<(i64, &str)> = listed
        .iter()
        .filter_map(|a| match a {
            DraftAttachment::Part { message, part } => Some((*message, part.as_str())),
            DraftAttachment::File { .. } => None,
        })
        .collect();
    let Some(draft) = existing else {
        return if listed.is_empty() {
            Ok(derived)
        } else {
            Err(not_this_drafts())
        };
    };
    let stored = &draft.attachments;
    if !listed
        .iter()
        .all(|&(m, p)| stored.iter().any(|a| is_part(a, m, p)))
    {
        return Err(not_this_drafts());
    }
    Ok(stored
        .iter()
        .filter(|a| listed.iter().any(|&(m, p)| is_part(a, m, p)))
        .cloned()
        .collect())
}

/// The draft to write: the existing one (the account's, not being sent) updated, or a new
/// one with a fresh `Message-ID`. An update keeps the forwarded parts the request lists; the
/// request's files replace its files.
fn compose_write(db: &Path, input: SaveInput) -> Result<DraftWrite, OpError> {
    let store = Store::open_reader(db)?;
    let SaveInput {
        account_id,
        address,
        request,
        recipients,
        files,
        provider,
    } = input;
    let existing = editable(&store, request.draft, account_id)?;
    let threading = match (&existing, request.reply_to.as_ref()) {
        // An update that names no message keeps what the draft was created with (the
        // message it answers may have left the store since).
        (Some(d), None) => kept_threading(d),
        (_, reply_to) => threading_for(&store, request.kind, reply_to, account_id)?,
    };
    let mut attachments = kept_parts(existing.as_ref(), &request.attachments, threading.parts)?;
    attachments.extend(files);
    check_total(request.body.len(), attachments.iter(), provider)?;
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
        source_remote_id: threading.source_remote_id,
    })
}

fn summary(attachment: &Attachment) -> AttachmentSummary {
    let (path, message, part) = match attachment {
        Attachment::File { path, .. } => (Some(path.clone()), None, None),
        Attachment::Part { message, part, .. } => (None, Some(*message), Some(part.clone())),
    };
    AttachmentSummary {
        name: attachment.name().to_string(),
        mime: attachment.mime().to_string(),
        size: attachment.size(),
        path,
        message,
        part,
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

pub(super) fn saved_id(outcome: OutboxOutcome) -> Result<DraftId, OpError> {
    match outcome {
        OutboxOutcome::DraftSaved(DraftSaved::Saved(id)) => Ok(id),
        OutboxOutcome::DraftSaved(DraftSaved::Missing) => Err(no_such_draft()),
        OutboxOutcome::DraftSaved(DraftSaved::Queued) => Err(being_sent()),
        OutboxOutcome::DraftSaved(DraftSaved::TokenUsed) => Err(token_used()),
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
        .action_target(&call.ctx, &request.account, Wants::Compose)
        .await?;
    let paths: Vec<String> = request
        .attachments
        .iter()
        .filter_map(|a| match a {
            DraftAttachment::File { path } => Some(path.clone()),
            DraftAttachment::Part { .. } => None,
        })
        .collect();
    let files = call.ctx.blocking(move || file_attachments(paths)).await?;
    let (db, client_token) = (call.ctx.paths.db_file(), request.client_token.clone());
    let input = SaveInput {
        account_id: target.account_id,
        address: target.address.clone(),
        request,
        recipients,
        files,
        provider: target.kind.provider_name(),
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
pub(super) fn draft_and_address(db: &Path, id: DraftId) -> Result<(Draft, String), OpError> {
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

/// Every file still there and readable, and the total still under the cap (the refusal names
/// `provider`).
fn check_sendable(draft: &Draft, provider: &str) -> Result<(), OpError> {
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
    check_total(draft.body.len(), files.iter().chain(parts), provider)
}

/// Why an enqueue wrote no row: the draft is being sent, or it is gone.
pub(super) fn not_queued(db: &Path, id: DraftId) -> Result<OpError, OpError> {
    let draft = Store::open_reader(db)?.draft(id)?;
    Ok(match draft {
        Some(d) if d.state == DraftState::Queued => {
            OpError::invalid_input("draft: this draft is already being sent")
        }
        _ => no_such_draft(),
    })
}

/// A token already in the outbox answers its row: refuse it unless that row is this draft's.
pub(super) fn row_of_draft(db: &Path, row: i64, id: DraftId) -> Result<(), OpError> {
    let owner = Store::open_reader(db)?
        .outbox_row(row)?
        .and_then(|r| r.draft_id);
    if owner != Some(id) {
        return Err(token_used());
    }
    Ok(())
}

/// The queued send of draft `id`, held for the undo grace.
fn send_action(account_id: i64, id: DraftId, client_token: String) -> NewAction {
    let grace = i64::try_from(UNDO_GRACE.as_secs()).unwrap_or(10);
    NewAction {
        account_id,
        action: Action::Send { draft: id },
        threads: Vec::new(),
        not_before: unix_now() + grace,
        client_token,
    }
}

pub(super) async fn send(
    call: Call,
    request: DraftSendRequest,
) -> Result<DraftSendResponse, OpError> {
    checked_client_token(&request.client_token)?;
    checked_id(request.draft)?;
    let (db, id) = (call.ctx.paths.db_file(), request.draft);
    let reader = db.clone();
    let (draft, address) = call
        .ctx
        .blocking(move || draft_and_address(&reader, id))
        .await?;
    let target = call
        .ctx
        .sync
        .action_target(&call.ctx, &address, Wants::Compose)
        .await?;
    if target.account_id != draft.account_id {
        return Err(no_such_draft());
    }
    let account_id = draft.account_id;
    let provider = target.kind.provider_name();
    call.ctx
        .blocking(move || check_sendable(&draft, provider))
        .await?;
    let new = send_action(account_id, id, request.client_token);
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

/// The import directory the draft owns: the one its own client token names, when it exists
/// (an import made it; tokens are unique across accounts). None when the draft is gone.
fn owned_files(db: &Path, paths: &Paths, id: DraftId) -> Result<Vec<PathBuf>, OpError> {
    let token = Store::open_reader(db)?.draft_client_token(id)?;
    Ok(token
        // A token that is not one plain path segment names no directory, so owns none.
        .and_then(|t| import_dir(paths, &t).ok())
        .filter(|dir| dir.is_dir())
        .into_iter()
        .collect())
}

/// The kind of account draft `id` belongs to; `None` when the draft is gone.
fn draft_owner(db: &Path, id: DraftId) -> Result<Option<AccountKind>, OpError> {
    let store = Store::open_reader(db)?;
    let Some(draft) = store.draft(id)? else {
        return Ok(None);
    };
    Ok(store
        .list_accounts()?
        .into_iter()
        .find(|a| a.id == draft.account_id)
        .map(|a| a.kind))
}

pub(super) async fn discard(
    call: Call,
    request: DraftDiscardRequest,
) -> Result<DraftDiscardResponse, OpError> {
    checked_id(request.draft)?;
    // The provider's name for a refusal; a draft already gone is a no-op.
    let (db, id) = (call.ctx.paths.db_file(), request.draft);
    let kind = call
        .ctx
        .blocking(move || draft_owner(&db, id))
        .await?
        .unwrap_or(AccountKind::Gmail);
    let (db, paths, id) = (
        call.ctx.paths.db_file(),
        call.ctx.paths.clone(),
        request.draft,
    );
    let owned = call
        .ctx
        .blocking(move || owned_files(&db, &paths, id))
        .await?;
    let outcome = call
        .ctx
        .sync
        .writer(&call.ctx)
        .await?
        .outbox(OutboxWrite::DraftDelete(request.draft))
        .await?;
    match outcome {
        OutboxOutcome::DraftDeleted(DraftDeleteOutcome::Deleted) => {}
        OutboxOutcome::DraftDeleted(DraftDeleteOutcome::BeingSent) => return Err(being_sent()),
        OutboxOutcome::DraftDeleted(DraftDeleteOutcome::BeingPushed) => {
            return Err(being_pushed(kind));
        }
        _ => return Err(unexpected()),
    }
    let files = remove_dirs(&owned);
    tracing::info!(draft = request.draft, files, "a draft was discarded");
    call.ctx.sync.outbox_changed(&call.ctx).await;
    Ok(DraftDiscardResponse {})
}
