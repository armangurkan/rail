//! Gmail labels as Rail folders. `UNREAD` and `STARRED` are flags, not folders; archive is
//! "no `INBOX` label", so it has no folder of its own.

use super::metadata::one_line_capped;
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
            one_line_capped(&label.name, MAX_NAME_CHARS),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Review minor 2: a user label's name is one line, capped, like envelope fields.
    #[test]
    fn a_label_name_is_one_line_and_capped() {
        let label = |name: &str| Label {
            id: "Label_1".into(),
            name: name.into(),
            kind: Some("user".into()),
        };
        let named = folder(label("Work\r\nInbox\u{7}")).unwrap();
        assert_eq!(named.name, "Work  Inbox ");
        let long = folder(label(&"y".repeat(5_000))).unwrap();
        assert_eq!(long.name.chars().count(), MAX_NAME_CHARS);
    }
}
