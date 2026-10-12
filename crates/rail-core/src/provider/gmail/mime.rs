//! The outgoing message as RFC 5322 bytes, built with `mail-builder`.

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;

use crate::provider::{Mailbox, Outgoing, ProviderError};

/// Gmail's limit for a message and its attachments.
pub const MAX_MESSAGE_BYTES: usize = 25 * 1024 * 1024;
const MAX_FILENAME_BYTES: usize = 255;
const DEFAULT_FILENAME: &str = "attachment";
/// Base64 grows data by 4/3; this is the per-part allowance for headers and boundaries.
const PART_OVERHEAD_BYTES: usize = 200;

/// Build `outgoing`; refuses a result over `MAX_MESSAGE_BYTES`.
pub fn build(outgoing: &Outgoing) -> Result<Vec<u8>, ProviderError> {
    let estimated: usize = outgoing.body.len()
        + outgoing
            .attachments
            .iter()
            .map(|a| a.bytes.len() / 3 * 4 + PART_OVERHEAD_BYTES)
            .sum::<usize>();
    if estimated > MAX_MESSAGE_BYTES {
        return Err(too_large());
    }
    let mut builder = MessageBuilder::new()
        .from(address(outgoing.from_name.as_deref(), &outgoing.from))
        .subject(outgoing.subject.as_str())
        .message_id(outgoing.message_id.trim_matches(['<', '>']))
        .text_body(outgoing.body.as_str());
    if !outgoing.to.is_empty() {
        builder = builder.to(list(&outgoing.to));
    }
    if !outgoing.cc.is_empty() {
        builder = builder.cc(list(&outgoing.cc));
    }
    if !outgoing.bcc.is_empty() {
        builder = builder.bcc(list(&outgoing.bcc));
    }
    if let Some(parent) = &outgoing.in_reply_to {
        builder = builder.in_reply_to(parent.trim_matches(['<', '>']));
    }
    if !outgoing.references.is_empty() {
        let refs: Vec<&str> = outgoing
            .references
            .iter()
            .map(|r| r.trim_matches(['<', '>']))
            .collect();
        builder = builder.references(refs);
    }
    for attachment in &outgoing.attachments {
        builder = builder.attachment(
            attachment.mime.as_str(),
            sanitize_filename(&attachment.name),
            attachment.bytes.as_slice(),
        );
    }
    let raw = builder.write_to_vec().map_err(|e| {
        ProviderError::permanent("Rail could not build the message").with_detail(e.to_string())
    })?;
    if raw.len() > MAX_MESSAGE_BYTES {
        return Err(too_large());
    }
    Ok(raw)
}

fn too_large() -> ProviderError {
    ProviderError::permanent("the message is larger than Gmail's 25 MiB limit")
}

fn address<'a>(name: Option<&'a str>, mailbox: &'a str) -> Address<'a> {
    Address::new_address(name.filter(|n| !n.trim().is_empty()), mailbox)
}

fn list(mailboxes: &[Mailbox]) -> Address<'_> {
    Address::new_list(
        mailboxes
            .iter()
            .map(|m| address(m.name.as_deref(), &m.address))
            .collect(),
    )
}

/// A file name safe in a MIME header: the last path segment, no control, format or bidi
/// characters, whitespace collapsed to single spaces, at most 255 bytes, never empty.
pub fn sanitize_filename(name: &str) -> String {
    let last = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let cleaned: String = last
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !c.is_control() && !crate::config::is_format_char(*c))
        .collect();
    let trimmed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out = if trimmed.is_empty() {
        DEFAULT_FILENAME.to_string()
    } else {
        trimmed
    };
    while out.len() > MAX_FILENAME_BYTES {
        out.pop();
    }
    out
}
