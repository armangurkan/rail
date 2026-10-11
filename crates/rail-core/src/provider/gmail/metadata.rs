//! `format=metadata` messages as envelopes. Mail is hostile input: each header value is cut to
//! `MAX_HEADER_BYTES` and loses its CR and LF before Rail rebuilds a header block for
//! `mail-parser`, so no value can start another header; decoded values are capped again.

use mail_parser::{Address, HeaderValue, MessageParser};

use super::labels::{DRAFT, STARRED, UNREAD};
use super::wire::{Header, Message};
use crate::provider::{Envelope, Mailbox};

/// The headers Rail asks Gmail for.
pub const METADATA_HEADERS: [&str; 9] = [
    "From",
    "To",
    "Cc",
    "Bcc",
    "Subject",
    "Date",
    "Message-ID",
    "In-Reply-To",
    "References",
];
pub const MAX_HEADER_BYTES: usize = 16 * 1024;
/// RFC 5322's line limit.
pub const MAX_SUBJECT_CHARS: usize = 998;
pub const MAX_ADDRESSES: usize = 100;
pub const MAX_SNIPPET_CHARS: usize = 300;
const MAX_NAME_CHARS: usize = 200;
const MAX_ADDRESS_CHARS: usize = 254;
const MAX_ID_CHARS: usize = 998;
const MAX_REFERENCES: usize = 50;
/// A `Date:` more than this far after Gmail received the message is not believed.
const MAX_DATE_SKEW_SECS: i64 = 366 * 86_400;

/// The envelope of `message`, or `None` when Rail cannot read it (no id, no thread, no usable
/// `internalDate`): the caller skips it by id.
pub(crate) fn envelope(message: Message) -> Option<Envelope> {
    let remote_id = message.id?;
    let thread_id = message.thread_id?;
    let internal_date: i64 = message.internal_date?.trim().parse().ok()?;
    let headers = message.payload.map(|p| p.headers).unwrap_or_default();
    let block = header_block(&headers);
    let parsed = MessageParser::default().parse(block.as_bytes());
    let labels = message.label_ids;
    let mut envelope = Envelope {
        remote_id,
        thread_id,
        internal_date,
        date: internal_date.div_euclid(1000),
        snippet: unescape_snippet(&message.snippet)
            .chars()
            .take(MAX_SNIPPET_CHARS)
            .collect(),
        is_read: !labels.iter().any(|l| l == UNREAD),
        is_starred: labels.iter().any(|l| l == STARRED),
        is_draft: labels.iter().any(|l| l == DRAFT),
        folders: labels
            .into_iter()
            .filter(|l| l != UNREAD && l != STARRED)
            .collect(),
        size: message.size_estimate.unwrap_or(0).max(0),
        ..Envelope::default()
    };
    if let Some(parsed) = parsed {
        fill_headers(&mut envelope, &parsed);
    }
    Some(envelope)
}

fn fill_headers(envelope: &mut Envelope, parsed: &mail_parser::Message<'_>) {
    envelope.subject = parsed
        .subject()
        .map(|s| one_line(s).chars().take(MAX_SUBJECT_CHARS).collect())
        .unwrap_or_default();
    envelope.from = mailboxes(parsed.from());
    envelope.to = mailboxes(parsed.to());
    envelope.cc = mailboxes(parsed.cc());
    envelope.bcc = mailboxes(parsed.bcc());
    envelope.message_id = parsed.message_id().map(capped_id);
    envelope.in_reply_to = texts(parsed.in_reply_to()).into_iter().next();
    envelope.references = texts(parsed.references());
    let received = envelope.internal_date.div_euclid(1000);
    if let Some(date) = parsed.date().map(|d| d.to_timestamp())
        && date > 0
        && date <= received + MAX_DATE_SKEW_SECS
    {
        envelope.date = date;
    }
}

/// `Name: value\r\n` for each wanted header (first occurrence), each value cut and on one line.
fn header_block(headers: &[Header]) -> String {
    let mut block = String::new();
    for wanted in METADATA_HEADERS {
        if let Some(header) = headers.iter().find(|h| h.name.eq_ignore_ascii_case(wanted)) {
            block.push_str(wanted);
            block.push_str(": ");
            block.push_str(&one_line(cut(&header.value, MAX_HEADER_BYTES)));
            block.push_str("\r\n");
        }
    }
    block.push_str("\r\n");
    block
}

/// At most `max` bytes of `text`, cut at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn one_line(text: &str) -> String {
    text.replace(['\r', '\n'], " ")
}

fn capped_id(id: &str) -> String {
    id.chars().take(MAX_ID_CHARS).collect()
}

fn mailboxes(address: Option<&Address<'_>>) -> Vec<Mailbox> {
    let Some(address) = address else {
        return Vec::new();
    };
    address
        .iter()
        .filter_map(|a| {
            let addr = a.address()?;
            Some(Mailbox {
                name: a
                    .name()
                    .map(|n| one_line(n).chars().take(MAX_NAME_CHARS).collect()),
                address: addr.chars().take(MAX_ADDRESS_CHARS).collect(),
            })
        })
        .take(MAX_ADDRESSES)
        .collect()
}

fn texts(value: &HeaderValue<'_>) -> Vec<String> {
    let all: Vec<String> = match (value.as_text_list(), value.as_text()) {
        (Some(list), _) => list.iter().map(|s| s.to_string()).collect(),
        (None, Some(one)) => vec![one.to_string()],
        (None, None) => Vec::new(),
    };
    all.into_iter()
        .take(MAX_REFERENCES)
        .map(|id| capped_id(&id))
        .collect()
}

/// Gmail's snippet carries HTML character references (`&#39;`, `&amp;`): decode those, and
/// leave anything else, including a bare `&`, as it is.
pub fn unescape_snippet(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        let decoded = tail
            .find(';')
            .filter(|end| *end <= 10)
            .and_then(|end| entity(&tail[1..end]).map(|c| (c, end)));
        match decoded {
            Some((c, end)) => {
                out.push(c);
                rest = &tail[end + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn entity(name: &str) -> Option<char> {
    match name {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        _ => {
            let number = name.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}
