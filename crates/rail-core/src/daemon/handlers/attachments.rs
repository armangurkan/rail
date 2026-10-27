//! `attachment.save` and `attachment.open`. A save writes one message's attachments into a
//! directory the client resolved; the answer is one outcome per requested part and the path
//! each file got — never bytes, and never a replaced file: a name already in the destination
//! becomes `2-name.pdf`. The destination is the user's own directory, so its mode is never
//! changed. An open writes one part into a private directory of its own under the cache,
//! named from the stored type rather than the sender's, and answers that path for the front
//! end to hand to the system opener; those copies are swept at daemon start and before every
//! open. Front-end operations only: no MCP tool reaches them. Logs carry counts and error
//! kinds, never a file name or a path.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::draft_files::file_name;
use crate::auth::random_token;
use crate::daemon::DaemonContext;
use crate::daemon::registry::Call;
use crate::error::ErrorClass;
use crate::ops::{
    AttachmentOpenRequest, AttachmentOpenResponse, AttachmentOutcome, AttachmentResult,
    AttachmentSaveRequest, AttachmentSaveResponse, ErrorCode, OpError, STALE_AFTER, openable,
    unwritable_dir,
};
use crate::paths::{Paths, create_private_dir};
use crate::provider::ProviderError;
use crate::provider::gmail::mime::{MAX_FILENAME_BYTES, sanitize_filename};
use crate::store::Store;
use crate::store::view::AttachmentView;
use crate::sync::manager::{Reach, SearchTarget};

/// A name that sanitizes to nothing, or that was only dots, dashes and spaces, becomes this.
const FALLBACK_NAME: &str = "attachment";
/// Held by every sweep, and by an open from its sweep through its write, so a sweep can never
/// remove the token directory an open has just made and make its write fail. The precedent is
/// `draft_files::IMPORTS`.
static OPENS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
/// How many names past the ones already seen a save tries before it gives up.
const EXTRA_NAME_TRIES: usize = 16;

pub(super) async fn save(
    call: Call,
    request: AttachmentSaveRequest,
) -> Result<AttachmentSaveResponse, OpError> {
    let named = request.checked_parts()?;
    // Every copy under Rail's own cache is swept a day after it was written (§7), so a save
    // into it would quietly lose the file. Refused before the account is reached, so nothing is
    // fetched and the cache is not created on the way.
    if Path::new(&request.dir).starts_with(call.ctx.paths.attachments_dir()) {
        return Err(destination_in_the_cache());
    }
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
/// then stripped of every leading dot, dash and space. A part called `.zshenv`, `.gitconfig`
/// or `.npmrc` cannot drop shell or tool configuration into a `$HOME` the user pointed
/// `attachment_dir` at, and one called `--checkpoint-action=exec=sh run.sh` cannot leave a
/// name a later glob would hand a command as an option. The space matters:
/// `sanitize_filename` keeps the one in `. .zshenv` and `file_name` sanitizes each candidate
/// again, which trims it, so dropping the dots alone still wrote a hidden file. What this
/// returns is a **fixed point** — it starts with none of the three, and sanitizing cannot put
/// one back — so no later sanitizing can uncover a leading dot. `create_new` guards only names
/// that already exist, and the `2-` prefix every later candidate carries is visible anyway. A
/// name of only dots, dashes and spaces becomes `attachment`.
fn visible_name(name: &str) -> String {
    let base = sanitize_filename(name);
    let visible = base.trim_start_matches(|c: char| c == '.' || c == '-' || c.is_whitespace());
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
    drop(file);
    if let Err(error) = quarantine(&path) {
        remove_written(&path);
        return (taken, Err(quarantine_error(&error)));
    }
    let size = i64::try_from(bytes.len()).unwrap_or(i64::MAX);
    (taken, Ok((text, size)))
}

/// Mark `path` as a download, so the system treats it as content that arrived from elsewhere
/// rather than as a file the user made: the flags, the time in lower-case hex and Rail as the
/// agent, written by `xattr` as an argument vector — never through a shell, and with no new
/// dependency and no `unsafe`.
#[cfg(target_os = "macos")]
fn quarantine(path: &Path) -> std::io::Result<()> {
    let value = format!("0083;{:x};Rail;", crate::auth::unix_now());
    let status = std::process::Command::new("/usr/bin/xattr")
        .arg("-w")
        .arg("com.apple.quarantine")
        .arg(&value)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        return Ok(());
    }
    Err(std::io::Error::other(
        "xattr could not write the quarantine attribute",
    ))
}

/// Nothing to do: only macOS reads a quarantine attribute, so elsewhere a written file is
/// already as marked as it is going to be.
#[cfg(not(target_os = "macos"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "one signature on both platforms, so the call sites do not branch"
)]
fn quarantine(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// A file Rail wrote that could not be marked as a download. Permanent: the next try would run
/// the same command against the same system. The file is never named.
fn quarantine_error(error: &std::io::Error) -> OpError {
    OpError::new(
        ErrorCode::Internal,
        ErrorClass::Permanent,
        "an attachment could not be marked as a download",
    )
    .with_detail(error.to_string())
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
        // A name this file system will not take (`ENAMETOOLONG` on Unix) comes from the
        // sender, so the next try would offer the same name again: permanent, not an I/O
        // hiccup to retry. The name is never quoted.
        ErrorKind::InvalidFilename => OpError::new(
            ErrorCode::InvalidInput,
            ErrorClass::Permanent,
            "this attachment's name is not one the file system takes",
        )
        .with_detail(error.to_string()),
        _ => OpError::io(ErrorCode::Internal, &format!("{what} an attachment"), error),
    }
}

pub(super) async fn open(
    call: Call,
    request: AttachmentOpenRequest,
) -> Result<AttachmentOpenResponse, OpError> {
    request.checked()?;
    let target = match call.ctx.sync.body_target(&request.account).await? {
        Reach::Ready(target) => target,
        Reach::Unavailable(error) => return Err(error),
    };
    // The writer opens (and migrates) the store; the handle is not kept.
    call.ctx.sync.writer(&call.ctx).await?;
    let listed = listed(&call.ctx, target.account_id, request.message_id.clone()).await?;
    let meta = listed
        .into_iter()
        .find(|meta| meta.part_id == request.part_id)
        // Never the part: it is the provider's text, and the front end already knows it.
        .ok_or_else(|| OpError::not_found("this message has no such attachment"))?;
    let extension = openable(&meta.mime_type).ok_or_else(unopenable)?;
    let bytes = target
        .provider
        .attachment(&request.message_id, &meta.part_id)
        .await?;
    let name = cache_name(&meta.filename, extension);
    let paths = call.ctx.paths.clone();
    // Taken before the sweep and held through the write.
    let _opens = OPENS.lock().await;
    let path = call
        .ctx
        .blocking(move || write_open_copy(&paths, &name, &bytes))
        .await?;
    Ok(AttachmentOpenResponse { path })
}

/// The refusal a destination inside `attachments_dir()` gets: the sweep removes what is under
/// it a day after it was written, so a save there would not keep. Names the field, never a
/// path.
fn destination_in_the_cache() -> OpError {
    OpError::invalid_input(
        "dir: Rail sweeps its own attachment cache, so a save into it would not keep",
    )
}

/// The refusal a type outside `OPENABLE` gets. Scripted and executable formats are not handed
/// to a program of the system's choosing.
fn unopenable() -> OpError {
    OpError::new(
        ErrorCode::Unsupported,
        ErrorClass::Permanent,
        "Rail does not open this attachment's type",
    )
}

/// The name an opened copy gets: the sanitized original's stem plus the extension the type
/// table gives, never the sender's own, and never past `MAX_FILENAME_BYTES`. On macOS `open`
/// dispatches on the extension, so an `.html` part declared `image/png` is handed over as
/// `.png`, not to a browser. The stem arrives already capped at the whole limit, so the
/// extension this adds has to be made room for, or the copy would be a file name the file
/// system refuses.
fn cache_name(filename: &str, extension: &str) -> String {
    let base = visible_name(filename);
    let stem = Path::new(&base)
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|stem| !stem.is_empty())
        .unwrap_or(FALLBACK_NAME);
    // Whole characters at a time, as `sanitize_filename` cuts. One byte of the stem always
    // stays, so the name can never come out as `.pdf`; every extension in `OPENABLE` is at
    // most four bytes, so the cut only ever takes the last few characters.
    let room = MAX_FILENAME_BYTES
        .saturating_sub(extension.len() + 1)
        .max(1);
    let mut stem = stem.to_string();
    while stem.len() > room {
        stem.pop();
    }
    format!("{stem}.{extension}")
}

/// Sweep the stale copies, then write `bytes` as a 0600 file named `name` in a fresh 0700
/// directory of its own under `attachments_dir()`; its path. A token directory per copy, so
/// removing one never touches another's.
fn write_open_copy(paths: &Paths, name: &str, bytes: &[u8]) -> Result<String, OpError> {
    if let Err(error) = sweep(paths, SystemTime::now()) {
        tracing::warn!(kind = ?error.kind(), "the opened attachments were not swept");
    }
    let dir = paths.attachments_dir().join(random_token());
    create_private_dir(&dir).map_err(|error| {
        OpError::io(ErrorCode::Internal, "creating a private directory", &error)
    })?;
    let path = dir.join(name);
    let Some(text) = path.to_str().map(ToString::to_string) else {
        return Err(OpError::invalid_input(
            "the cache directory's path is not valid UTF-8",
        ));
    };
    // A failure leaves the token directory behind otherwise: it holds no file for the sweep
    // to find stale, so nothing would ever remove it.
    let mut file = match create_new_private(&path) {
        Ok(file) => file,
        Err(error) => {
            drop_if_empty(&dir);
            return Err(write_error("creating", &error));
        }
    };
    if let Err(error) = write_synced(&mut file, bytes) {
        drop(file);
        remove_written(&path);
        drop_if_empty(&dir);
        return Err(write_error("writing", &error));
    }
    drop(file);
    if let Err(error) = quarantine(&path) {
        remove_written(&path);
        drop_if_empty(&dir);
        return Err(quarantine_error(&error));
    }
    Ok(text)
}

/// Remove every file under `attachments_dir()` last changed before `now - STALE_AFTER`, then
/// every token directory that is left empty; how many files went. Each entry's own type is
/// checked, so a symlink is never followed and nothing outside the directory is touched. No
/// directory yet means nothing to do. An entry that has gone since it was listed is skipped —
/// the reader removes the copies it opened, and the sweep before an open runs beside the one at
/// start — and any other failure is kept while the rest of the directory is swept, the first of
/// them coming back at the end.
pub(super) fn sweep(paths: &Paths, now: SystemTime) -> std::io::Result<usize> {
    let root = paths.attachments_dir();
    let tokens = match std::fs::read_dir(&root) {
        Ok(tokens) => tokens,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let cutoff = now
        .checked_sub(STALE_AFTER)
        .unwrap_or(SystemTime::UNIX_EPOCH);
    let mut removed = 0;
    let mut failed = None;
    for token in tokens {
        match token.and_then(|token| sweep_entry(&token, cutoff)) {
            Ok(went) => removed += went,
            Err(error) => keep_first(&mut failed, error),
        }
    }
    first_error(failed, removed)
}

/// One entry of the attachments directory: the stale files in a token directory, and the
/// directory itself once the sweep has left it empty. Anything that is not a directory — a
/// symlink included, which is never followed — is left alone.
fn sweep_entry(token: &std::fs::DirEntry, cutoff: SystemTime) -> std::io::Result<usize> {
    if !token.file_type()?.is_dir() {
        return Ok(0);
    }
    let removed = sweep_token(&token.path(), cutoff)?;
    drop_if_empty(&token.path());
    Ok(removed)
}

/// Remove the regular files in one token directory last changed before `cutoff`; how many. A
/// directory that is no longer there has nothing stale in it, and a file that has gone since it
/// was listed is not a failure either; the first failure that is neither comes back once the
/// rest of the directory has been swept.
pub(super) fn sweep_token(dir: &Path, cutoff: SystemTime) -> std::io::Result<usize> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    let mut removed = 0;
    let mut failed = None;
    for entry in entries {
        match entry.and_then(|entry| sweep_file(&entry, cutoff)) {
            Ok(went) => removed += went,
            Err(error) => keep_first(&mut failed, error),
        }
    }
    first_error(failed, removed)
}

/// One entry of a token directory: removed when it is a regular file last changed before
/// `cutoff`, and 1 when it went.
fn sweep_file(entry: &std::fs::DirEntry, cutoff: SystemTime) -> std::io::Result<usize> {
    if !entry.file_type()?.is_file() || entry.metadata()?.modified()? >= cutoff {
        return Ok(0);
    }
    std::fs::remove_file(entry.path())?;
    Ok(1)
}

/// Keep `error` as what the sweep will report, unless it is an entry that has gone (there is
/// nothing left to do about it) or a failure is already kept (the first one is reported).
fn keep_first(failed: &mut Option<std::io::Error>, error: std::io::Error) {
    if error.kind() == std::io::ErrorKind::NotFound || failed.is_some() {
        return;
    }
    *failed = Some(error);
}

/// What one sweep loop answers: the failure it kept, else how many files it removed.
fn first_error(failed: Option<std::io::Error>, removed: usize) -> std::io::Result<usize> {
    match failed {
        Some(error) => Err(error),
        None => Ok(removed),
    }
}

/// Remove `dir` when nothing is left in it. A directory still holding a fresh copy, and one
/// already gone, are both fine.
fn drop_if_empty(dir: &Path) {
    use std::io::ErrorKind;
    match std::fs::remove_dir(dir) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
            ) => {}
        Err(error) => {
            tracing::warn!(kind = ?error.kind(), "an opened attachment's directory stayed");
        }
    }
}

/// The start-up sweep, in the background: a copy a crashed terminal left behind does not
/// survive until the next open. Nothing to do without the directory, which `sweep` answers
/// with 0 — the question is asked on the blocking pool with the rest of the sweep's I/O, never
/// on the runtime.
pub(crate) async fn sweep_at_start(ctx: std::sync::Arc<DaemonContext>) {
    let _opens = OPENS.lock().await;
    let paths = ctx.paths.clone();
    match ctx
        .blocking(move || Ok(sweep(&paths, SystemTime::now())))
        .await
    {
        Ok(Ok(removed)) => tracing::info!(removed, "opened attachments swept"),
        Ok(Err(error)) => {
            tracing::warn!(kind = ?error.kind(), "the opened attachments were not swept");
        }
        Err(error) => tracing::warn!(code = ?error.code, "the opened attachments were not swept"),
    }
}
