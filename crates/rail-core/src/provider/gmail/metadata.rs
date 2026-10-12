//! `format=metadata` messages as envelopes. Mail is hostile input: each header value is cut to
//! `MAX_HEADER_BYTES` and loses its control characters (CR and LF among them) before Rail
//! rebuilds a header block for `mail-parser`, so no value can start another header; decoded
//! values are flattened to one line and capped again. Ids are checked like any other input.

use mail_parser::{Address, HeaderValue, MessageParser};

use super::checked_id;
use super::labels::{DRAFT, STARRED, UNREAD};
use super::wire::{Header, Message};
use crate::provider::{Envelope, Mailbox};

/// The headers Rail asks Gmail for.
pub const METADATA_HEADERS: [&str; 10] = [
    "From",
    "To",
    "Cc",
    "Bcc",
    "Reply-To",
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
/// A `Date:` more than this far after (or before) Gmail received the message is not believed.
const MAX_DATE_SKEW_SECS: i64 = 366 * 86_400;
/// Label ids kept per message.
const MAX_LABELS: usize = 100;

/// The envelope of `message`, or `None` when Rail cannot read it (no id or thread id safe in a
/// URL path, no usable `internalDate`): the caller skips it by id. Label ids that are not safe
/// are dropped, and at most `MAX_LABELS` are kept.
pub(crate) fn envelope(message: Message) -> Option<Envelope> {
    let remote_id = message.id.filter(|id| checked_id(id).is_some())?;
    let thread_id = message.thread_id.filter(|id| checked_id(id).is_some())?;
    let internal_date: i64 = message.internal_date?.trim().parse().ok()?;
    let headers = message.payload.map(|p| p.headers).unwrap_or_default();
    let block = header_block(&headers);
    let parsed = MessageParser::default().parse(block.as_bytes());
    let labels: Vec<String> = message
        .label_ids
        .into_iter()
        .filter(|l| checked_id(l).is_some())
        .take(MAX_LABELS)
        .collect();
    let mut envelope = Envelope {
        remote_id,
        thread_id,
        internal_date,
        date: internal_date.div_euclid(1000),
        snippet: one_line(&unescape_snippet(&message.snippet))
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
        .map(|s| one_line_capped(s, MAX_SUBJECT_CHARS))
        .unwrap_or_default();
    envelope.from = mailboxes(parsed.from());
    envelope.to = mailboxes(parsed.to());
    envelope.cc = mailboxes(parsed.cc());
    envelope.bcc = mailboxes(parsed.bcc());
    envelope.reply_to = mailboxes(parsed.reply_to());
    envelope.message_id = parsed.message_id().map(capped_id);
    envelope.in_reply_to = texts(parsed.in_reply_to()).into_iter().next();
    envelope.references = texts(parsed.references());
    let received = envelope.internal_date.div_euclid(1000);
    if let Some(date) = parsed.date().and_then(|d| believed(d, received)) {
        envelope.date = date;
    }
}

/// `date` in Unix seconds when every field is in range (year 1900-3000) and it lies within
/// `MAX_DATE_SKEW_SECS` of `received`; else `None`, and the receive time stands.
fn believed(date: &mail_parser::DateTime, received: i64) -> Option<i64> {
    if !date.is_valid() || date.day > days_in_month(date.year, date.month) || date.tz_hour > 14 {
        return None;
    }
    let seconds = date.to_timestamp();
    (seconds.abs_diff(received) <= MAX_DATE_SKEW_SECS.unsigned_abs()).then_some(seconds)
}

fn days_in_month(year: u16, month: u8) -> u8 {
    let leap = (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400);
    match month {
        2 if leap => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
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
pub(super) fn cut(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// `text`'s characters with every control character (CR, LF, NUL, ESC, C1...) a space: the one
/// rule that puts header text on one line.
fn one_line_chars(text: &str) -> impl Iterator<Item = char> + '_ {
    text.chars().map(|c| if c.is_control() { ' ' } else { c })
}

/// `text` on one line (each control character a space).
fn one_line(text: &str) -> String {
    one_line_chars(text).collect()
}

/// The first `max` characters of `text` on one line (each control character a space).
pub(super) fn one_line_capped(text: &str, max: usize) -> String {
    one_line_chars(text).take(max).collect()
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
                name: a.name().map(|n| one_line_capped(n, MAX_NAME_CHARS)),
                address: one_line_capped(addr, MAX_ADDRESS_CHARS),
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

/// The furthest a snippet reference's `;` may stand from its `&` (`&#x10FFFF;` is 10).
const MAX_REFERENCE_END: usize = 10;

/// Gmail's snippet carries HTML character references (`&#39;`, `&amp;`): decode those, and
/// leave anything else, including a bare `&` and a reference to a control character below
/// U+0020 or DEL, as it is.
pub fn unescape_snippet(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let tail = &rest[start..];
        // The `;` is looked for only where a reference Rail decodes could end, so a run of
        // bare `&` costs one pass, not one scan of the rest each.
        let decoded = tail
            .bytes()
            .take(MAX_REFERENCE_END + 1)
            .position(|b| b == b';')
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
            if code < 0x20 || code == 0x7f {
                return None;
            }
            char::from_u32(code)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One rule for header text: `one_line_capped` is `one_line` cut to `max` characters,
    /// for controls, C1, line separators, wide and combining characters alike.
    #[test]
    fn one_line_capped_is_one_line_cut_to_max_characters() {
        let hostile = [
            "plain",
            "a\r\nb\0c\u{1b}[2Jd",
            "\u{85}next\u{9b}line",
            "日本語のメール",
            "e\u{301}e\u{301}e\u{301}",
            "",
        ];
        for text in hostile {
            for max in [0, 1, 3, 100] {
                let expected: String = one_line(text).chars().take(max).collect();
                assert_eq!(one_line_capped(text, max), expected, "{text:?} {max}");
            }
        }
        assert_eq!(one_line("a\nb\u{7f}c"), "a b c");
        assert_eq!(one_line_capped("ab\ncd", 3), "ab ");
    }
}
