//! The files an imported Gmail draft owns. `draft.import` fetches each attachment once into
//! `Paths::draft_files_dir()/<client token>` (0700, files 0600), so later pushes and the send
//! never depend on Gmail's copy of the source message, which the first push replaces.
//! An import never takes a directory a draft names. `draft.discard` removes the directory
//! the draft's own token names, and so does a completed send (the outbox
//! worker, best effort); at daemon start every directory no draft names is swept, which
//! catches what a failed removal left. Logs carry counts, never a file name.

use std::collections::HashSet;
use std::path::{Component, Path, PathBuf};

use super::drafts::token_used;
use crate::daemon::DaemonContext;
use crate::ops::{ErrorCode, OpError};
use crate::paths::{Paths, create_private_dir, remove_dir_if_present};
use crate::provider::gmail::mime::sanitize_filename;
use crate::provider::{Provider, ProviderError};
use crate::store::Store;
use crate::store::drafts::{Attachment, ReplyAttachment};

/// Held by an import from its token check to its store write, and by the start sweep: a
/// directory being filled is never swept, and two imports never share one.
pub(crate) static IMPORTS: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A name that is not a file name (`.`, `..`) becomes this.
const FALLBACK_NAME: &str = "attachment";

fn io_error(what: &str, e: &std::io::Error) -> OpError {
    OpError::io(ErrorCode::Internal, what, e)
}

/// `name` as a file name in one import's directory: one path segment, never `.` or `..`, and
/// unlike (ignoring case) every name `taken` holds, by a `2-`, `3-`… prefix.
pub(crate) fn file_name(name: &str, taken: &mut HashSet<String>) -> String {
    let base = match sanitize_filename(name) {
        n if n == "." || n == ".." => FALLBACK_NAME.to_string(),
        n => n,
    };
    let mut candidate = base.clone();
    let mut n = 2;
    while !taken.insert(candidate.to_lowercase()) {
        candidate = sanitize_filename(&format!("{n}-{base}"));
        n += 1;
    }
    candidate
}

/// The directory an import with `token` fills; `token` passed `checked_token`.
pub(crate) fn import_dir(paths: &Paths, token: &str) -> Result<PathBuf, OpError> {
    paths
        .import_dir(token)
        .ok_or_else(|| OpError::invalid_input("client_token: not a valid token"))
}

/// The import directory `path` lies in: exactly `draft_files_dir()/<dir>/<file>`.
fn owning_dir(root: &Path, path: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(root).ok()?;
    let parts: Vec<Component<'_>> = rest.components().collect();
    match parts.as_slice() {
        [Component::Normal(dir), Component::Normal(_)] => Some(root.join(dir)),
        _ => None,
    }
}

/// Remove `dirs`; how many went. A failure is logged and the sweep at the next start retries.
pub(crate) fn remove_dirs(dirs: &[PathBuf]) -> usize {
    dirs.iter()
        .filter(|dir| match remove_dir_if_present(dir) {
            Ok(removed) => removed,
            Err(e) => {
                tracing::warn!(error_kind = ?e.kind(), "a draft's files could not be removed");
                false
            }
        })
        .count()
}

/// One attachment fetched from the provider.
struct Fetched {
    meta: ReplyAttachment,
    bytes: Vec<u8>,
}

async fn fetch_all(
    provider: &dyn Provider,
    remote: &str,
    parts: Vec<ReplyAttachment>,
    provider_name: &str,
) -> Result<Vec<Fetched>, OpError> {
    let mut fetched = Vec::with_capacity(parts.len());
    for meta in parts {
        // The name is the provider's text and stays out of the message.
        let bytes = provider
            .attachment(&remote.to_string(), &meta.part_id)
            .await
            .map_err(|e| match e {
                ProviderError::NotFound => OpError::not_found(format!(
                    "an attachment of this draft is no longer in {provider_name}"
                )),
                other => other.into(),
            })?;
        fetched.push(Fetched { meta, bytes });
    }
    Ok(fetched)
}

/// Take `dir` for this import: refused when any draft names a file in it (or the store
/// cannot say), so another draft's files are never cleared; else an earlier failed try's
/// leftovers go and the directory is made fresh (0700).
fn claim(db: &Path, root: &Path, dir: &Path) -> Result<(), OpError> {
    let referenced = Store::open_reader(db)?.draft_file_paths()?;
    let named = referenced.is_none_or(|paths| {
        paths
            .iter()
            .any(|p| owning_dir(root, Path::new(p)).as_deref() == Some(dir))
    });
    if named {
        return Err(token_used());
    }
    match std::fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(io_error("clearing an import's files", &e));
        }
        _ => {}
    }
    create_private_dir(dir).map_err(|e| io_error("creating an import's directory", &e))
}

/// One new file, 0600, written and synced. The directory is fresh and private, so no temp
/// name is needed: a name is never longer than the attachment's own.
fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Write `fetched` into the claimed `dir`; the file entries.
fn write_all(dir: &Path, fetched: Vec<Fetched>) -> Result<Vec<Attachment>, OpError> {
    let mut taken = HashSet::new();
    let mut files = Vec::with_capacity(fetched.len());
    for Fetched { meta, bytes } in fetched {
        let path = dir.join(file_name(&meta.filename, &mut taken));
        write_new(&path, &bytes).map_err(|e| io_error("writing an imported attachment", &e))?;
        let path = path.to_str().ok_or_else(|| {
            OpError::invalid_input("the data directory's path is not valid UTF-8")
        })?;
        files.push(Attachment::File {
            path: path.to_string(),
            name: meta.filename,
            mime: meta.mime_type,
            size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        });
    }
    std::fs::File::open(dir)
        .and_then(|d| d.sync_all())
        .map_err(|e| io_error("syncing an import's directory", &e))?;
    Ok(files)
}

/// Claim `dir`, fetch every part of the provider's message `remote` and store it there; the file
/// entries. Once claimed, any failure removes `dir` and fails the import.
pub(crate) async fn fetch_into(
    ctx: &DaemonContext,
    provider: &dyn Provider,
    remote: &str,
    parts: Vec<ReplyAttachment>,
    dir: PathBuf,
    provider_name: &str,
) -> Result<Vec<Attachment>, OpError> {
    let (db, root, claimed) = (
        ctx.paths.db_file(),
        ctx.paths.draft_files_dir(),
        dir.clone(),
    );
    ctx.blocking(move || claim(&db, &root, &claimed)).await?;
    let fetched = match fetch_all(provider, remote, parts, provider_name).await {
        Ok(fetched) => fetched,
        Err(e) => {
            remove_dirs(std::slice::from_ref(&dir));
            return Err(e);
        }
    };
    let target = dir.clone();
    let written = ctx.blocking(move || write_all(&target, fetched)).await;
    if written.is_err() {
        remove_dirs(std::slice::from_ref(&dir));
    }
    written
}

/// Remove every directory under `root` that no path of `referenced` lies in; how many went.
/// Only directories are touched, and a symlink is never followed.
pub(crate) fn sweep(paths: &Paths, referenced: &[String]) -> std::io::Result<usize> {
    let root = paths.draft_files_dir();
    let kept: HashSet<PathBuf> = referenced
        .iter()
        .filter_map(|p| owning_dir(&root, Path::new(p)))
        .collect();
    let mut stale = Vec::new();
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() && !kept.contains(&entry.path()) {
            stale.push(entry.path());
        }
    }
    Ok(remove_dirs(&stale))
}

/// The start-up sweep, in the background: nothing to do without the directory; a store that
/// cannot say which files its drafts name keeps everything.
pub(crate) async fn sweep_at_start(ctx: std::sync::Arc<DaemonContext>) {
    if !ctx.paths.draft_files_dir().exists() {
        return;
    }
    let _imports = IMPORTS.lock().await;
    // The writer opens (and migrates) the store; the handle is not kept.
    if let Err(e) = ctx.sync.writer(&ctx).await {
        tracing::warn!(code = ?e.code, "the draft files were not swept");
        return;
    }
    let (db, paths) = (ctx.paths.db_file(), ctx.paths.clone());
    let swept = ctx
        .blocking(move || {
            let Some(referenced) = Store::open_reader(&db)?.draft_file_paths()? else {
                return Ok(None);
            };
            sweep(&paths, &referenced)
                .map(Some)
                .map_err(|e| io_error("sweeping the draft files", &e))
        })
        .await;
    match swept {
        Ok(Some(removed)) => tracing::info!(removed, "draft files swept"),
        Ok(None) => tracing::warn!("a draft's attachments cannot be read; nothing was swept"),
        Err(e) => tracing::warn!(code = ?e.code, "the draft files were not swept"),
    }
}
