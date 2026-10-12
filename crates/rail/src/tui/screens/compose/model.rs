//! What the composer holds: the draft's headers as typed, its body and its attachments.

use std::fmt;

use rail_core::ops::DraftId;
use rail_core::store::drafts::DraftKind;

/// The four headers of the compose file, as typed (`parse_recipient` validates them). Every
/// string may be mail content, so `Debug` prints counts only.
#[derive(Default, Clone, PartialEq, Eq)]
pub struct Headers {
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
}

impl fmt::Debug for Headers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Headers")
            .field("to", &self.to.len())
            .field("cc", &self.cc.len())
            .field("bcc", &self.bcc.len())
            .field("subject_len", &self.subject.len())
            .finish()
    }
}

/// One attachment of the draft: a local file (`path`) or a part of the forwarded message
/// (`path: None`; `message` and `part` name it once the daemon has stored it).
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentRow {
    pub name: String,
    pub mime: String,
    pub size: i64,
    pub path: Option<String>,
    /// The forwarded message's store id, for a part.
    pub message: Option<i64>,
    /// The part id, for a part.
    pub part: Option<String>,
}

impl fmt::Debug for AttachmentRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentRow")
            .field("size", &self.size)
            .field("local", &self.path.is_some())
            .finish_non_exhaustive()
    }
}

/// The draft being written.
#[derive(Clone, PartialEq, Eq)]
pub struct Composing {
    /// `None` until the daemon first saves it.
    pub draft: Option<DraftId>,
    pub account_index: usize,
    pub account_address: String,
    pub kind: DraftKind,
    /// The store's id of the message replied to or forwarded.
    pub reply_to: Option<i64>,
    pub headers: Headers,
    pub body: String,
    pub attachments: Vec<AttachmentRow>,
    pub message_id: Option<String>,
    /// The last refusal, shown on the review.
    pub errors: Vec<String>,
}

impl Composing {
    /// The attachments' sizes added up.
    pub fn total_size(&self) -> i64 {
        self.attachments.iter().map(|a| a.size.max(0)).sum()
    }

    /// To, Cc and Bcc in that order.
    pub fn recipients(&self) -> Vec<String> {
        let h = &self.headers;
        h.to.iter().chain(&h.cc).chain(&h.bcc).cloned().collect()
    }
}

impl fmt::Debug for Composing {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Composing")
            .field("draft", &self.draft)
            .field("account_index", &self.account_index)
            .field("kind", &self.kind)
            .field("reply_to", &self.reply_to)
            .field("headers", &self.headers)
            .field("body_len", &self.body.len())
            .field("attachments", &self.attachments.len())
            .field("errors", &self.errors.len())
            .finish_non_exhaustive()
    }
}
