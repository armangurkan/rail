//! The outgoing message as RFC 5322 bytes, built with `mail-builder`.

use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;

use crate::provider::{Mailbox, Outgoing, ProviderError};

/// Gmail's limit for a message and its attachments.
pub const MAX_MESSAGE_BYTES: usize = 25 * 1024 * 1024;
const MAX_FILENAME_BYTES: usize = 255;
const MIB: usize = 1024 * 1024;
const DEFAULT_FILENAME: &str = "attachment";
/// Base64 grows data by 4/3; this is the per-part allowance for headers and boundaries.
const PART_OVERHEAD_BYTES: usize = 200;

/// Build `outgoing`; refuses a result over `MAX_MESSAGE_BYTES`.
pub fn build(outgoing: &Outgoing) -> Result<Vec<u8>, ProviderError> {
    validate(outgoing)?;
    let estimated: usize = outgoing.body.len()
        + outgoing
            .attachments
            .iter()
            .map(|a| a.bytes.len() / 3 * 4 + PART_OVERHEAD_BYTES)
            .sum::<usize>();
    if estimated > MAX_MESSAGE_BYTES {
        return Err(too_large(estimated));
    }
    let mut builder = MessageBuilder::new()
        .from(address(outgoing.from_name.as_deref(), &outgoing.from))
        .subject(strip_controls(&outgoing.subject))
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
        return Err(too_large(raw.len()));
    }
    Ok(raw)
}

fn too_large(bytes: usize) -> ProviderError {
    let tenths = bytes.saturating_mul(10) / MIB;
    ProviderError::permanent(&format!(
        "the message is {}.{} MiB, larger than Gmail's 25 MiB limit",
        tenths / 10,
        tenths % 10
    ))
}

fn strip_controls(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

fn address<'a>(name: Option<&'a str>, mailbox: &'a str) -> Address<'a> {
    let name = name.map(strip_controls).filter(|n| !n.trim().is_empty());
    Address::new_address(name, mailbox)
}

/// Refuse, before anything is built, every field the library writes onto the wire as given:
/// addresses, message ids and MIME types. A line break in one would add a header. Messages
/// name the field, never the value.
fn validate(outgoing: &Outgoing) -> Result<(), ProviderError> {
    let addresses = std::iter::once(outgoing.from.as_str())
        .chain(outgoing.to.iter().map(|m| m.address.as_str()))
        .chain(outgoing.cc.iter().map(|m| m.address.as_str()))
        .chain(outgoing.bcc.iter().map(|m| m.address.as_str()));
    for mailbox in addresses {
        if !valid_address(mailbox) {
            return Err(ProviderError::permanent(
                "an address is not a plain address",
            ));
        }
    }
    let ids = std::iter::once(outgoing.message_id.as_str())
        .chain(outgoing.in_reply_to.as_deref())
        .chain(outgoing.references.iter().map(String::as_str));
    for id in ids {
        if !valid_message_id(id) {
            return Err(ProviderError::permanent("a message id is not a plain id"));
        }
    }
    if outgoing.attachments.iter().any(|a| !valid_mime(&a.mime)) {
        return Err(ProviderError::permanent(
            "an attachment's type is not valid",
        ));
    }
    Ok(())
}

fn plain_text(text: &str) -> bool {
    !text.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn valid_address(address: &str) -> bool {
    let mut parts = address.split('@');
    matches!(
        (parts.next(), parts.next(), parts.next()),
        (Some(local), Some(domain), None) if !local.is_empty() && !domain.is_empty()
    ) && plain_text(address)
}

fn valid_message_id(id: &str) -> bool {
    let inner = id.strip_prefix('<').unwrap_or(id);
    let inner = inner.strip_suffix('>').unwrap_or(inner);
    plain_text(inner) && !inner.contains(['<', '>']) && valid_address(inner)
}

/// `token/token` (RFC 2045): printable ASCII but for `()<>@,;:\"/[]?=`.
fn valid_mime(mime: &str) -> bool {
    let token = |t: &str| {
        !t.is_empty()
            && t.bytes()
                .all(|b| b.is_ascii_graphic() && !b"()<>@,;:\\\"/[]?=".contains(&b))
    };
    matches!(mime.split_once('/'), Some((kind, sub)) if token(kind) && token(sub))
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
