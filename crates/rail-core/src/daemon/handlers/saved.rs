//! `saved_search.put` and `saved_search.remove`: the `[[saved_searches]]` tables of
//! `config.toml`. Only the daemon writes them, and only under the config lock.
//!
//! The order of the checks is pinned, not incidental: the bare `handlers/remove.rs` idiom would
//! let `config::edit`'s own `ConfigError::Invalid` surface every refusal as `ErrorCode::Config`
//! with the reason buried in `detail`, so a clashing name and an unknown name would be
//! indistinguishable. Each refusal is therefore decided here, before the edit, and the edit's
//! own validation stays a defensive second line.

use std::path::{Path, PathBuf};

use crate::config::{
    SavedSearch, check_name, check_put, duplicate_name, load, load_text_or_empty, name_rule,
    put_saved_search, remove_saved_search,
};
use crate::daemon::context::DaemonContext;
use crate::daemon::registry::Call;
use crate::error::ErrorClass;
use crate::ops::{
    ErrorCode, OpError, SavedSearchListResponse, SavedSearchPutRequest, SavedSearchRemoveRequest,
};
use crate::paths::{create_private_dir, replace_private_atomic};

/// Which edit one request asks for. Both go through the same locked read-edit-write.
enum Edit {
    Put {
        name: String,
        /// `None` keeps the query of the entry `replaces` names (#109).
        query: Option<String>,
        replaces: Option<String>,
    },
    Remove {
        name: String,
    },
}

impl Edit {
    /// Every name the request carries, so step 1 can check them all before any lookup.
    fn names(&self) -> Vec<&str> {
        match self {
            Self::Put { name, replaces, .. } => {
                let mut all = vec![name.as_str()];
                all.extend(replaces.as_deref());
                all
            }
            Self::Remove { name } => vec![name.as_str()],
        }
    }
}

pub(super) async fn put(
    call: Call,
    request: SavedSearchPutRequest,
) -> Result<SavedSearchListResponse, OpError> {
    apply(
        call,
        Edit::Put {
            name: request.name,
            query: request.query,
            replaces: request.replaces,
        },
    )
    .await
}

pub(super) async fn remove(
    call: Call,
    request: SavedSearchRemoveRequest,
) -> Result<SavedSearchListResponse, OpError> {
    apply(call, Edit::Remove { name: request.name }).await
}

async fn apply(call: Call, edit: Edit) -> Result<SavedSearchListResponse, OpError> {
    // Step 1: every name in the request is well-formed before anything is read, so a name
    // quoted back in a `NotFound` message can only ever be a well-formed one.
    if edit.names().into_iter().any(|n| check_name(n).is_err()) {
        return Err(OpError::invalid_input(name_rule()));
    }
    call.ctx.write_locked(move |ctx| locked(ctx, &edit)).await
}

/// Steps 2 to 7, holding the cross-process config lock for all of them (FR-23), so a
/// `[general]` toggle in the TUI cannot interleave its own read-edit-rename with this one.
fn locked(ctx: &DaemonContext, edit: &Edit) -> Result<SavedSearchListResponse, OpError> {
    let target = resolve(&ctx.paths.config_file())?;
    let (text, config) = load_text_or_empty(&target)?;
    let out = edited(&text, &target, &config.saved_searches, edit)?;
    write_back(&target, &out)?;
    Ok(SavedSearchListResponse {
        searches: load(&target)?.saved_searches,
    })
}

/// Steps 3 to 5: refuse what must be refused, with its own code, then make the edit.
fn edited(
    text: &str,
    target: &Path,
    existing: &[SavedSearch],
    edit: &Edit,
) -> Result<String, OpError> {
    match edit {
        Edit::Remove { name } => {
            if !holds(existing, name) {
                return Err(unknown(name));
            }
            Ok(remove_saved_search(text, target, name)?)
        }
        Edit::Put {
            name,
            query,
            replaces,
        } => put_edit(
            text,
            target,
            existing,
            name,
            query.as_deref(),
            replaces.as_deref(),
        ),
    }
}

/// Steps 3 to 5 for a put. A rename without a query keeps the file's own: the client's copy
/// of the list can be older than a hand edit made while it was open, and writing it back would
/// lose that edit (#109). The query checked is the one the file will hold.
fn put_edit(
    text: &str,
    target: &Path,
    existing: &[SavedSearch],
    name: &str,
    query: Option<&str>,
    replaces: Option<&str>,
) -> Result<String, OpError> {
    let current = match replaces {
        Some(old) => Some(
            entry(existing, old)
                .ok_or_else(|| unknown(old))?
                .query
                .as_str(),
        ),
        None => None,
    };
    let Some(checked) = query.or(current) else {
        return Err(OpError::invalid_input("A new saved search needs a query."));
    };
    if let Err(reason) = check_put(existing, name, checked, replaces) {
        // One rule, asked twice: `check_put` writes the sentence, `duplicate_name`
        // says whether it was the clash, so the code matches the sentence.
        let code = if duplicate_name(existing, name, replaces).is_some() {
            ErrorCode::Duplicate
        } else {
            ErrorCode::InvalidInput
        };
        return Err(OpError::new(code, ErrorClass::Permanent, reason));
    }
    Ok(put_saved_search(text, target, name, query, replaces)?)
}

/// Step 6. Not `ops::error::write_config_text`: that goes through `write_private_atomic`,
/// which calls `create_private_dir` and would tighten an existing config directory to 0700 —
/// on a dotfile-managed one that is a change Rail has no business making.
fn write_back(target: &Path, out: &str) -> Result<(), OpError> {
    if let Some(parent) = target
        .parent()
        .filter(|d| !d.as_os_str().is_empty() && !d.is_dir())
    {
        create_private_dir(parent).map_err(|e| io_failed("creating", parent, &e))?;
    }
    replace_private_atomic(target, out.as_bytes()).map_err(|e| io_failed("writing", target, &e))
}

/// Step 2: a symlinked `config.toml` is written where it points, so a dotfiles setup keeps its
/// link instead of having a regular file renamed over it. The same sequence `write_general`
/// uses on the TUI side.
fn resolve(path: &Path) -> Result<PathBuf, OpError> {
    let is_link = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    if is_link {
        std::fs::canonicalize(path).map_err(|e| io_failed("reading", path, &e))
    } else {
        Ok(path.to_path_buf())
    }
}

/// True when `existing` holds `name`, ignoring ASCII case — the rule the loader makes saved
/// search names unique by, so at most one entry can match.
fn holds(existing: &[SavedSearch], name: &str) -> bool {
    entry(existing, name).is_some()
}

/// The entry `existing` holds under `name`, ignoring ASCII case.
fn entry<'a>(existing: &'a [SavedSearch], name: &str) -> Option<&'a SavedSearch> {
    existing.iter().find(|s| s.name.eq_ignore_ascii_case(name))
}

fn unknown(name: &str) -> OpError {
    OpError::not_found(format!("There is no saved search named {name:?}."))
}

fn io_failed(what: &str, path: &Path, error: &std::io::Error) -> OpError {
    OpError::io(
        ErrorCode::Config,
        &format!("{what} {}", path.display()),
        error,
    )
}
