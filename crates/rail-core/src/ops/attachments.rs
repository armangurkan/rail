//! `attachment.save` and `attachment.open`: the two terminal-only operations that put one of a
//! message's attachments on disk. Both answer a **path**, never bytes: a frame is 1 MiB
//! (`daemon::wire::MAX_FRAME_BYTES`) and an attachment is routinely larger, so the daemon
//! writes the file and the front end is told where it landed. Nothing here prints a filename,
//! a part id, a directory or a path: every `Debug` is hand-written and counts only.

use std::collections::HashSet;
use std::fmt;
use std::path::Path;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::error::OpError;

/// Part ids one `attachment.save` may name. An empty list is bounded instead by the store's
/// own cap on how many attachments a message lists (`provider::limits::MAX_ATTACHMENTS`).
pub const MAX_SAVE_PARTS: usize = 50;
/// The longest part id, and the longest message id, Rail passes on, in bytes. Gmail's part ids
/// are dotted (`0.1`) and a Graph id is URL-safe base64 that may end `=`, so neither fits
/// `BodyFetchRequest::checked_ids`' letters-digits-`-`-`_` rule; the provider's own gate
/// (`gmail::body::part_id_ok`, `graph::graph_id`) decides after this one.
pub const MAX_PART_ID_BYTES: usize = 512;
/// The longest destination directory, in bytes.
pub const MAX_DIR_BYTES: usize = 4096;

/// How long an opened copy under `Paths::attachments_dir()` may live. Defined here, beside
/// `OPENABLE`, because the sweeper is a `rail-core` daemon handler and `rail-core` cannot
/// depend on the `rail` binary: the TUI re-exports this constant, so there is one value.
pub const STALE_AFTER: Duration = Duration::from_secs(86_400);

/// Types Rail hands to the system opener, with the extension it writes for each. Scripted and
/// executable formats are deliberately absent — `text/html`, `image/svg+xml`, shell scripts,
/// `.desktop`, `.app`, `.pkg`, `.dmg`, `.jar`, `.scpt` — because opening runs a program of the
/// system's choosing on bytes a stranger sent. `text/markdown` is absent too: common Markdown
/// viewers render embedded HTML in a WebView, which would undo the `text/html` exclusion.
pub const OPENABLE: [(&str, &str); 21] = [
    ("application/pdf", "pdf"),
    ("text/plain", "txt"),
    ("text/csv", "csv"),
    ("text/tab-separated-values", "tsv"),
    ("application/rtf", "rtf"),
    ("application/msword", "doc"),
    (
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "docx",
    ),
    ("application/vnd.ms-excel", "xls"),
    (
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xlsx",
    ),
    ("application/vnd.ms-powerpoint", "ppt"),
    (
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "pptx",
    ),
    ("application/vnd.oasis.opendocument.text", "odt"),
    ("application/vnd.oasis.opendocument.spreadsheet", "ods"),
    ("application/vnd.oasis.opendocument.presentation", "odp"),
    ("image/png", "png"),
    ("image/jpeg", "jpg"),
    ("image/gif", "gif"),
    ("image/webp", "webp"),
    ("image/bmp", "bmp"),
    ("image/tiff", "tif"),
    ("image/heic", "heic"),
];

/// The extension `OPENABLE` gives for `mime_type`, parameters stripped and case ignored
/// (`text/plain; charset=utf-8` matches `text/plain`). `None` means Rail does not open it.
#[must_use]
pub fn openable(mime_type: &str) -> Option<&'static str> {
    let base = mime_type.split(';').next().unwrap_or_default().trim();
    OPENABLE
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(base))
        .map(|(_, extension)| *extension)
}

/// One id's own rule: 1 to `MAX_PART_ID_BYTES` bytes, with no control and no format
/// character, so an unbounded or invisible id never reaches SQLite or a provider URL. The
/// provider's own gate decides after this one. `what` names the field; the value is never
/// quoted.
fn checked_id(value: &str, what: &str) -> Result<(), OpError> {
    let plain = !value
        .chars()
        .any(|c| c.is_control() || crate::config::is_format_char(c));
    if value.is_empty() || value.len() > MAX_PART_ID_BYTES || !plain {
        return Err(OpError::invalid_input(format!(
            "{what}: 1 to {MAX_PART_ID_BYTES} bytes, with no control or format characters"
        )));
    }
    Ok(())
}

/// `attachment.save`: write the named parts of one message into `dir`.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachmentSaveRequest {
    /// An account's name or address.
    pub account: String,
    /// The provider's message id, checked by the part-id rule above.
    pub message_id: String,
    /// Empty means every attachment the store lists for that message.
    pub part_ids: Vec<String>,
    /// An absolute directory, resolved by the client: the daemon's working directory is `/`.
    pub dir: String,
}

/// What happened to each requested part.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentSaveResponse {
    /// One per distinct part asked for, in the order asked (store order when none were named).
    pub results: Vec<AttachmentResult>,
}

/// One part's outcome.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentResult {
    pub part_id: String,
    pub outcome: AttachmentOutcome,
}

/// How one part's save ended.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentOutcome {
    /// Written as a new 0600 file; `path` is the name it actually got, which a clash with an
    /// existing name turns into `2-name.pdf`.
    Saved { path: String, size: i64 },
    /// The store does not list this part, or the provider no longer has it.
    Missing,
    /// The fetch or the write failed; `class()` says whether retrying may help.
    Failed(OpError),
}

/// `attachment.open`: write one part into a private directory under the cache and answer its
/// path, for the front end to hand to the system opener.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttachmentOpenRequest {
    /// An account's name or address.
    pub account: String,
    /// The provider's message id, checked by the part-id rule above.
    pub message_id: String,
    pub part_id: String,
}

/// Where the copy landed: 0600, in a 0700 directory named by a random token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentOpenResponse {
    pub path: String,
}

impl AttachmentSaveRequest {
    /// The part ids, each once, in the order asked, with the request's own limits checked
    /// first. Empty stays empty: the handler expands it from the store.
    ///
    /// # Errors
    /// `InvalidInput`/`Permanent`, quoting nothing of the request, when the destination is not
    /// absolute, too many parts are named, or a part or message id breaks the part-id rule.
    pub fn checked_parts(&self) -> Result<Vec<String>, OpError> {
        if !Path::new(&self.dir).is_absolute() {
            return Err(not_absolute_dir());
        }
        if self.dir.len() > MAX_DIR_BYTES {
            return Err(OpError::invalid_input(format!(
                "dir: at most {MAX_DIR_BYTES} bytes"
            )));
        }
        checked_id(&self.message_id, "message_id")?;
        if self.part_ids.len() > MAX_SAVE_PARTS {
            return Err(OpError::invalid_input(format!(
                "attachment.save takes at most {MAX_SAVE_PARTS} part ids"
            )));
        }
        let mut seen = HashSet::new();
        let mut parts = Vec::with_capacity(self.part_ids.len());
        for id in &self.part_ids {
            checked_id(id, "a part id")?;
            if seen.insert(id.as_str()) {
                parts.push(id.clone());
            }
        }
        Ok(parts)
    }
}

impl AttachmentOpenRequest {
    /// Check the message and part ids against the part-id rule.
    ///
    /// # Errors
    /// `InvalidInput`/`Permanent`, quoting neither id.
    pub fn checked(&self) -> Result<(), OpError> {
        checked_id(&self.message_id, "message_id")?;
        checked_id(&self.part_id, "a part id")
    }
}

/// The refusal a destination that is not absolute gets. Names the field, never a file.
#[must_use]
pub fn not_absolute_dir() -> OpError {
    OpError::invalid_input("dir: not an absolute path")
}

/// The refusal a destination Rail cannot write gets: a configuration problem, so permanent.
/// The operating system's own words go to `detail`, which is outside text.
#[must_use]
pub fn unwritable_dir(detail: &str) -> OpError {
    OpError::invalid_input("dir: cannot be written").with_detail(detail)
}

/// Counts only: a request names an account, a message and its parts, all of which are mail.
impl fmt::Debug for AttachmentSaveRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentSaveRequest")
            .field("parts", &self.part_ids.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for AttachmentSaveResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentSaveResponse")
            .field("results", &self.results.len())
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for AttachmentResult {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentResult")
            .field("outcome", &self.outcome)
            .finish_non_exhaustive()
    }
}

/// The variant's name and, for a save, the size. Never the path, and never the error's text:
/// a provider's words may quote the attachment.
impl fmt::Debug for AttachmentOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Saved { size, .. } => f
                .debug_struct("Saved")
                .field("size", size)
                .finish_non_exhaustive(),
            Self::Missing => f.write_str("Missing"),
            Self::Failed(_) => f.debug_struct("Failed").finish_non_exhaustive(),
        }
    }
}

impl fmt::Debug for AttachmentOpenRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentOpenRequest")
            .finish_non_exhaustive()
    }
}

impl fmt::Debug for AttachmentOpenResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentOpenResponse")
            .finish_non_exhaustive()
    }
}
