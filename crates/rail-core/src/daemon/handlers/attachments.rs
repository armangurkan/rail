//! `attachment.save`: one message's attachments written into a directory the client resolved.
//! The answer is one outcome per requested part and the path each file got — never bytes, and
//! never a replaced file: a name already in the destination becomes `2-name.pdf`. The
//! destination is the user's own directory, so its mode is never changed. Front-end
//! operations only: no MCP tool reaches them. Logs carry counts and error kinds, never a file
//! name or a path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::draft_files::file_name;
use crate::daemon::DaemonContext;
use crate::daemon::registry::Call;
use crate::ops::{
    AttachmentOutcome, AttachmentResult, AttachmentSaveRequest, AttachmentSaveResponse, ErrorCode,
    OpError, unwritable_dir,
};
use crate::provider::ProviderError;
use crate::provider::gmail::mime::sanitize_filename;
use crate::store::Store;
use crate::store::view::AttachmentView;
use crate::sync::manager::{Reach, SearchTarget};

/// A name that sanitizes to nothing, or that was only dots, becomes this.
const FALLBACK_NAME: &str = "attachment";
/// How many names past the ones already seen a save tries before it gives up.
const EXTRA_NAME_TRIES: usize = 16;

pub(super) async fn save(
    call: Call,
    request: AttachmentSaveRequest,
) -> Result<AttachmentSaveResponse, OpError> {
    let named = request.checked_parts()?;
    let target = match call.ctx.sync.body_target(&request.account).await? {
        Reach::Ready(target) => target,
        // With an empty part list the parts are known only after a store read, and that read
        // needs the account id only this arm's target carries: there is nothing to fan an
        // outcome out over, and `Ok { results: [] }` would leave the front end with no line.
        Reach::Unavailable(error) => return Err(error),
    };
    // The writer opens (and migrates) the store; the handle is not kept.
    call.ctx.sync.writer(&call.ctx).await?;
    let listed = listed(&call.ctx, target.account_id, request.message_id.clone()).await?;
    let dir = PathBuf::from(&request.dir);
    let mut taken = {
        let dir = dir.clone();
        call.ctx.blocking(move || prepare(&dir)).await?
    };
    let mut results = Vec::with_capacity(listed.len().max(named.len()));
    for (part_id, meta) in wanted(&named, &listed) {
        let outcome = match meta {
            None => AttachmentOutcome::Missing,
            Some(meta) => {
                let (back, outcome) =
                    save_one(&call.ctx, &target, &request.message_id, meta, &dir, taken).await;
                taken = back;
                outcome
            }
        };
        results.push(AttachmentResult {
            part_id: part_id.to_string(),
            outcome,
        });
    }
    tracing::info!(parts = results.len(), "attachments saved");
    Ok(AttachmentSaveResponse { results })
}

/// Every attachment the store lists for the account's message. The only source of a part's
/// filename, type and size: Graph's `$value` answers bytes with no metadata.
pub(super) async fn listed(
    ctx: &DaemonContext,
    account_id: i64,
    message_id: String,
) -> Result<Vec<AttachmentView>, OpError> {
    let db = ctx.paths.db_file();
    ctx.blocking(move || Ok(Store::open_reader(&db)?.listed_attachments(account_id, &message_id)?))
        .await
}

/// One entry per requested part, in the order asked: the store's listing for it, or `None`
/// when the store does not list it. An empty request is every listed part, in store order.
fn wanted<'a>(
    named: &'a [String],
    listed: &'a [AttachmentView],
) -> Vec<(&'a str, Option<&'a AttachmentView>)> {
    if named.is_empty() {
        return listed
            .iter()
            .map(|meta| (meta.part_id.as_str(), Some(meta)))
            .collect();
    }
    named
        .iter()
        .map(|id| (id.as_str(), listed.iter().find(|meta| meta.part_id == *id)))
        .collect()
}

/// Fetch one part and write it; the name set as the write left it, and the outcome. A part the
/// provider no longer has is `Missing`; any other provider or write failure is `Failed`, and
/// the loop carries on with the next part.
async fn save_one(
    ctx: &DaemonContext,
    target: &SearchTarget,
    message_id: &str,
    meta: &AttachmentView,
    dir: &Path,
    taken: HashSet<String>,
) -> (HashSet<String>, AttachmentOutcome) {
    let fetched = target
        .provider
        .attachment(&message_id.to_string(), &meta.part_id)
        .await;
    let bytes = match fetched {
        Ok(bytes) => bytes,
        Err(ProviderError::NotFound) => return (taken, AttachmentOutcome::Missing),
        Err(other) => return (taken, AttachmentOutcome::Failed(other.into())),
    };
    let (dir, name) = (dir.to_path_buf(), visible_name(&meta.filename));
    let written = ctx
        .blocking(move || Ok(write_part(&dir, &name, &bytes, taken)))
        .await;
    match written {
        Ok((taken, Ok((path, size)))) => (taken, AttachmentOutcome::Saved { path, size }),
        Ok((taken, Err(error))) => (taken, AttachmentOutcome::Failed(error)),
        // The blocking pool itself failed, so the name set is gone with it.
        Err(error) => (HashSet::new(), AttachmentOutcome::Failed(error)),
    }
}

/// `name` as a visible file name in the user's own directory: sanitized to one path segment,
/// then stripped of its leading dots, so a part called `.zshenv`, `.gitconfig` or `.npmrc`
/// cannot drop shell or tool configuration into a `$HOME` the user pointed `attachment_dir`
/// at. `create_new` guards only names that already exist, and the `2-` prefix every later
/// candidate carries is visible anyway. A name that was only dots becomes `attachment`.
fn visible_name(name: &str) -> String {
    let base = sanitize_filename(name);
    let visible = base.trim_start_matches('.');
    if visible.is_empty() {
        FALLBACK_NAME.to_string()
    } else {
        visible.to_string()
    }
}

/// Write `bytes` as a new 0600 file named after `name` in `dir`, never replacing a name
/// `taken` holds or one already on disk; the name set as this write left it, and the path and
/// size written. A write that fails after the file was created removes the file Rail created;
/// a create that fails removes nothing.
fn write_part(
    dir: &Path,
    name: &str,
    bytes: &[u8],
    mut taken: HashSet<String>,
) -> (HashSet<String>, Result<(String, i64), OpError>) {
    let created = create_new_in(dir, name, &mut taken);
    let (path, mut file) = match created {
        Ok(created) => created,
        Err(error) => return (taken, Err(write_error("creating", &error))),
    };
    let Some(text) = path.to_str().map(ToString::to_string) else {
        remove_written(&path);
        let message = "the destination's path is not valid UTF-8";
        return (taken, Err(OpError::invalid_input(message)));
    };
    if let Err(error) = write_synced(&mut file, bytes) {
        drop(file);
        remove_written(&path);
        return (taken, Err(write_error("writing", &error)));
    }
    let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    (taken, Ok((text, size)))
}

/// A fresh 0600 file in `dir` for `name`, by `file_name`'s `2-`, `3-`… dedup over `taken`
/// plus `create_new`, which never writes through an existing file or a symlink. An
/// `AlreadyExists` steps to the next suffix: the snapshot `prepare` took can be stale, and
/// another writer may have taken the name since.
fn create_new_in(
    dir: &Path,
    name: &str,
    taken: &mut HashSet<String>,
) -> std::io::Result<(PathBuf, std::fs::File)> {
    let tries = taken.len() + EXTRA_NAME_TRIES;
    let mut last = None;
    for _ in 0..tries {
        let path = dir.join(file_name(name, taken));
        match create_new_private(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => last = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "no unused name in the destination",
        )
    }))
}

/// A new file, owner-only from the moment it exists; never an existing file or a symlink.
pub(super) fn create_new_private(path: &Path) -> std::io::Result<std::fs::File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
    }
}

/// `bytes` written and on the disk.
pub(super) fn write_synced(file: &mut std::fs::File, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Remove a file Rail created and could not finish. Logged by error kind only: the name is
/// the sender's.
fn remove_written(path: &Path) {
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            tracing::warn!(kind = ?error.kind(), "a part-written attachment could not be removed");
        }
    }
}

/// Make sure `dir` can take files, and snapshot the names already in it (lower-cased) so a
/// save never replaces one. `create_private_dir` is deliberately not used: `dir` is the
/// user's own directory and tightening `~/Downloads` to 0700 is not Rail's to do, so a
/// missing destination is created at the process umask and an existing one is left alone.
fn prepare(dir: &Path) -> Result<HashSet<String>, OpError> {
    std::fs::create_dir_all(dir).map_err(|error| dir_error(&error))?;
    let meta = std::fs::metadata(dir).map_err(|error| dir_error(&error))?;
    if !meta.is_dir() || meta.permissions().readonly() {
        return Err(unwritable_dir());
    }
    let mut taken = HashSet::new();
    for entry in std::fs::read_dir(dir).map_err(|error| dir_error(&error))? {
        let entry = entry.map_err(|error| dir_error(&error))?;
        taken.insert(entry.file_name().to_string_lossy().to_lowercase());
    }
    Ok(taken)
}

/// An I/O failure on the user's own destination. The kinds that mean "this directory cannot
/// take files" are a configuration problem, so permanent and named; anything else is Rail's
/// own trouble and stays a transient I/O error.
fn dir_error(error: &std::io::Error) -> OpError {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::PermissionDenied
        | ErrorKind::NotADirectory
        | ErrorKind::AlreadyExists
        | ErrorKind::NotFound
        | ErrorKind::ReadOnlyFilesystem => unwritable_dir().with_detail(error.to_string()),
        _ => OpError::io(ErrorCode::Internal, "preparing the destination", error),
    }
}

/// A failure writing one part: the user's directory may be the cause, so the same
/// classification as `dir_error`. `what` never names the file.
fn write_error(what: &str, error: &std::io::Error) -> OpError {
    use std::io::ErrorKind;
    match error.kind() {
        ErrorKind::PermissionDenied
        | ErrorKind::NotADirectory
        | ErrorKind::ReadOnlyFilesystem
        | ErrorKind::AlreadyExists => unwritable_dir().with_detail(error.to_string()),
        _ => OpError::io(ErrorCode::Internal, &format!("{what} an attachment"), error),
    }
}
