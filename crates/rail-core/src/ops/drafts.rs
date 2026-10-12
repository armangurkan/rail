//! `draft.save`, `draft.send`, `draft.discard`: the requests, their bounds, and the checks
//! that run before anything is stored. Every refusal names its field.

use std::collections::HashSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::actions::{DraftId, OutboxId, checked_token};
use super::error::OpError;
use crate::config::{is_format_char, is_plain};
use crate::store::drafts::{DraftKind, Recipient};

pub const MAX_RECIPIENTS: usize = 100;
pub const MAX_ATTACHMENTS: usize = 50;
pub const MAX_SUBJECT_BYTES: usize = 998;
pub const MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
pub const MAX_ADDRESS_CHARS: usize = 254;
pub const MAX_NAME_CHARS: usize = 200;
pub const MAX_TOTAL_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;

/// A refused recipient is quoted (with `{:?}`) only up to this many characters.
const MAX_QUOTED_CHARS: usize = MAX_ADDRESS_CHARS + MAX_NAME_CHARS + 8;

/// The message a reply or forward answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplyTo {
    /// The store's message id.
    pub message: i64,
}

/// An attachment as the TUI names it: a file by path. (Parts of forwarded messages are added
/// by the daemon from the store.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DraftAttachment {
    File { path: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftSaveRequest {
    pub account: String,
    /// Update this draft; absent creates one.
    #[serde(default)]
    pub draft: Option<DraftId>,
    pub kind: DraftKind,
    #[serde(default)]
    pub reply_to: Option<ReplyTo>,
    /// Each `address` or `Name <address>`.
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub attachments: Vec<DraftAttachment>,
    /// A create sent again with the same token answers the draft it made.
    pub client_token: String,
}

/// An attachment as the daemon stored it (its name, type and size are what the review shows).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AttachmentSummary {
    pub name: String,
    pub mime: String,
    pub size: i64,
    /// The file's path for a file attachment; `None` for a forwarded part.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftSaveResponse {
    pub draft: DraftId,
    pub message_id: String,
    /// Every attachment of the saved draft, in order.
    #[serde(default)]
    pub attachments: Vec<AttachmentSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftSendRequest {
    pub draft: DraftId,
    /// Sent again with the same token, the request answers the row it queued.
    pub client_token: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftSendResponse {
    /// The queued row: `action.undo` with it and the same token takes the send back.
    pub row: OutboxId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftDiscardRequest {
    pub draft: DraftId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct DraftDiscardResponse {}

/// The checked recipients of one request (`Debug` shows no address).
#[derive(Debug)]
pub struct CheckedRecipients {
    pub to: Vec<Recipient>,
    pub cc: Vec<Recipient>,
    pub bcc: Vec<Recipient>,
}

fn invalid(field: &str, why: &str) -> OpError {
    OpError::invalid_input(format!("{field}: {why}"))
}

fn has_hidden(text: &str) -> bool {
    text.chars().any(|c| c.is_control() || is_format_char(c))
}

/// `address` or `Name <address>`; the name may be quoted. The address is an RFC 5322
/// addr-spec with a dotted domain of letters, digits and hyphens (or `xn--` labels), and no
/// RFC 5322 special in it: what the MIME builder accepts. The error never quotes the text.
pub fn parse_recipient(text: &str) -> Result<Recipient, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("an address is empty".to_string());
    }
    if has_hidden(text) {
        return Err("an address holds a control or format character".to_string());
    }
    let (name, address) = match (text.rfind('<'), text.ends_with('>')) {
        (Some(open), true) => (
            display_name(text[..open].trim())?,
            &text[open + 1..text.len() - 1],
        ),
        (Some(_), false) | (None, true) => return Err("an address is malformed".to_string()),
        (None, false) => (None, text),
    };
    check_address(address)?;
    Ok(Recipient {
        name,
        address: address.to_string(),
    })
}

/// The name before `<address>`: quoted (anything but `"` and `\` inside), or bare (no `"`,
/// `<` or `>`). `None` when empty.
fn display_name(raw: &str) -> Result<Option<String>, String> {
    let quoted = raw.len() >= 2 && raw.starts_with('"') && raw.ends_with('"');
    let name = if quoted { &raw[1..raw.len() - 1] } else { raw };
    let refused: &[char] = if quoted {
        &['"', '\\']
    } else {
        &['"', '<', '>', '\\']
    };
    if name.contains(refused) {
        return Err("a name holds a quote, a backslash or an angle bracket".to_string());
    }
    if name.chars().count() > MAX_NAME_CHARS {
        return Err(format!("a name is longer than {MAX_NAME_CHARS} characters"));
    }
    let name = name.trim();
    Ok((!name.is_empty()).then(|| name.to_string()))
}

fn check_address(address: &str) -> Result<(), String> {
    if address.chars().count() > MAX_ADDRESS_CHARS {
        return Err(format!(
            "an address is longer than {MAX_ADDRESS_CHARS} characters"
        ));
    }
    let Some((local, domain)) = address.split_once('@') else {
        return Err("an address has no @".to_string());
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') || !is_plain(address) {
        return Err("an address is malformed".to_string());
    }
    // atext and dots only: none of ( ) < > [ ] : ; \ , " can reach the header.
    if local
        .bytes()
        .any(|b| !(b.is_ascii_alphanumeric() || b"!#$%&'*+-/=?^_`{|}~.".contains(&b)))
        || local.starts_with('.')
        || local.ends_with('.')
        || local.contains("..")
    {
        return Err("an address is malformed".to_string());
    }
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 || !labels.iter().all(|l| domain_label(l)) {
        return Err("an address's domain is malformed".to_string());
    }
    Ok(())
}

fn domain_label(label: &str) -> bool {
    !label.is_empty()
        && !label.starts_with('-')
        && !label.ends_with('-')
        && label.len() <= 63
        && label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn check_list(field: &str, list: &[String]) -> Result<Vec<Recipient>, OpError> {
    list.iter()
        .map(|item| {
            parse_recipient(item).map_err(|why| {
                if item.chars().count() <= MAX_QUOTED_CHARS {
                    invalid(field, &format!("{item:?}: {why}"))
                } else {
                    invalid(field, &why)
                }
            })
        })
        .collect()
}

impl DraftSaveRequest {
    /// Every bound of spec §5 that needs no file system and no store.
    pub fn checked(&self) -> Result<CheckedRecipients, OpError> {
        checked_token(&self.client_token).map_err(|e| invalid("client_token", &e.message))?;
        if self.draft.is_some_and(|d| d <= 0) {
            return Err(invalid("draft", "a draft id is a positive number"));
        }
        let total = self.to.len() + self.cc.len() + self.bcc.len();
        if total > MAX_RECIPIENTS {
            return Err(invalid(
                "recipients",
                &format!("at most {MAX_RECIPIENTS} recipients"),
            ));
        }
        let recipients = CheckedRecipients {
            to: check_list("to", &self.to)?,
            cc: check_list("cc", &self.cc)?,
            bcc: check_list("bcc", &self.bcc)?,
        };
        if self.subject.contains(['\n', '\r']) || self.subject.len() > MAX_SUBJECT_BYTES {
            return Err(invalid(
                "subject",
                &format!("one line of at most {MAX_SUBJECT_BYTES} bytes"),
            ));
        }
        if has_hidden(&self.subject) {
            return Err(invalid("subject", "holds a control or format character"));
        }
        if self.body.len() > MAX_BODY_BYTES {
            return Err(invalid("body", "larger than 10 MiB"));
        }
        self.checked_attachments()?;
        let answers = matches!(
            self.kind,
            DraftKind::Reply | DraftKind::ReplyAll | DraftKind::Forward
        );
        // An update without `reply_to` keeps the threading the draft was created with.
        if answers && self.reply_to.is_none() && self.draft.is_none() {
            return Err(invalid(
                "reply_to",
                "a reply or forward names the message it answers",
            ));
        }
        Ok(recipients)
    }

    fn checked_attachments(&self) -> Result<(), OpError> {
        if self.attachments.len() > MAX_ATTACHMENTS {
            return Err(invalid(
                "attachments",
                &format!("at most {MAX_ATTACHMENTS} attachments"),
            ));
        }
        let mut seen = HashSet::new();
        for DraftAttachment::File { path } in &self.attachments {
            if !std::path::Path::new(path).is_absolute() {
                return Err(invalid("attachments", "a path must be absolute"));
            }
            if !seen.insert(path) {
                return Err(invalid("attachments", "a file is attached twice"));
            }
        }
        Ok(())
    }
}

/// `<uuid@domain>`, the domain taken from `address` (`rail.invalid` when it has none).
pub fn new_message_id(address: &str) -> String {
    let domain = address
        .rsplit_once('@')
        .map_or("rail.invalid", |(_, d)| d)
        .to_ascii_lowercase();
    format!("<{}@{domain}>", uuid::Uuid::new_v4())
}
