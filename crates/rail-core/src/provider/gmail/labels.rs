//! Gmail labels as Rail folders. `UNREAD` and `STARRED` are flags, not folders; archive is
//! "no `INBOX` label", so it has no folder of its own.

use super::wire::Label;
use crate::provider::{FolderKind, RemoteFolder};

/// Labels Rail reads as flags.
pub(crate) const UNREAD: &str = "UNREAD";
pub(crate) const STARRED: &str = "STARRED";
pub(crate) const DRAFT: &str = "DRAFT";
const MAX_NAME_CHARS: usize = 200;

pub(super) fn folder(label: Label) -> Option<RemoteFolder> {
    if label.id == UNREAD || label.id == STARRED {
        return None;
    }
    let system = label.kind.as_deref() == Some("system");
    let (kind, name) = match label.id.as_str() {
        "INBOX" => (FolderKind::Inbox, "Inbox".to_string()),
        "SENT" => (FolderKind::Sent, "Sent".to_string()),
        DRAFT => (FolderKind::Drafts, "Drafts".to_string()),
        "TRASH" => (FolderKind::Trash, "Trash".to_string()),
        "SPAM" => (FolderKind::Spam, "Spam".to_string()),
        id if system => (FolderKind::Label, system_name(id)),
        _ => (
            FolderKind::User,
            label.name.chars().take(MAX_NAME_CHARS).collect(),
        ),
    };
    Some(RemoteFolder {
        remote_id: label.id,
        name,
        kind,
    })
}

/// `IMPORTANT` → `Important`, `CATEGORY_SOCIAL` → `Category: Social`.
fn system_name(id: &str) -> String {
    let title = |word: &str| {
        let lower = word.to_ascii_lowercase();
        let mut chars = lower.chars();
        chars
            .next()
            .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
            .unwrap_or_default()
    };
    match id.strip_prefix("CATEGORY_") {
        Some(category) => format!("Category: {}", title(category)),
        None => title(id),
    }
}
