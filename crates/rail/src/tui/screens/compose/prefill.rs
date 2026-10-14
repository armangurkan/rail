//! What a reply or a forward starts with: recipients, subject, the quoted or forwarded text with
//! its links, and the signature.

use std::collections::BTreeSet;

use jiff::Timestamp;
use jiff::tz::TimeZone;
use rail_core::provider::Mailbox;
use rail_core::store::drafts::ReplySource;

use super::file::{one_line, plain_line_ends};
use super::model::Headers;
use crate::tui::text::{markers, safe_link};

const FORWARD_BAR: &str = "---------- Forwarded message ---------";
/// Characters that make a display name need quotes.
const NAME_SPECIALS: [char; 3] = [',', '<', '>'];
/// What a link line says for an address Rail would not open.
const NOT_INCLUDED: &str = "(link not included)";

/// `Name <address>`, or `address` alone when there is no name, controls replaced by spaces.
/// Quotes and backslashes are dropped from the name (a quoted name may not hold them) and a name holding `,`, `<` or `>`
/// is quoted.
pub fn display_parts(name: Option<&str>, address: &str) -> String {
    let cleaned: String = one_line(name.unwrap_or(""))
        .chars()
        .filter(|c| !matches!(c, '"' | '\\'))
        .collect();
    let cleaned = cleaned.trim();
    let address = one_line(address);
    if cleaned.is_empty() {
        return address;
    }
    if cleaned.contains(NAME_SPECIALS) || name.is_some_and(|n| n.contains('"')) {
        format!("\"{cleaned}\" <{address}>")
    } else {
        format!("{cleaned} <{address}>")
    }
}

/// A mailbox as a recipient line shows it.
pub fn display(m: &Mailbox) -> String {
    display_parts(m.name.as_deref(), &m.address)
}

fn starts_with_any(subject: &str, prefixes: &[&str]) -> bool {
    let lower = subject.trim_start().to_lowercase();
    prefixes.iter().any(|p| lower.starts_with(p))
}

/// `Re: ` once: a subject already starting with `Re:` in any case is kept.
pub fn re_subject(subject: &str) -> String {
    let subject = one_line(subject);
    if starts_with_any(&subject, &["re:"]) {
        subject
    } else {
        format!("Re: {subject}")
    }
}

/// `Fwd: ` once: a subject already starting with `Fwd:` or `Fw:` in any case is kept.
pub fn fwd_subject(subject: &str) -> String {
    let subject = one_line(subject);
    if starts_with_any(&subject, &["fwd:", "fw:"]) {
        subject
    } else {
        format!("Fwd: {subject}")
    }
}

/// `text`, then a blank line and one line per link marker it holds, by number: `[n] url` for
/// a link `safe_link` accepts, `[n] (link not included)` for one it refuses. Links the text
/// never marks are left out; a text with no marker comes back as it is.
pub fn with_links(text: &str, links: &[String]) -> String {
    let named: BTreeSet<usize> = text
        .lines()
        .flat_map(|line| markers(line, links.len()))
        .map(|m| m.number)
        .collect();
    if named.is_empty() {
        return text.to_string();
    }
    let mut out = text.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push('\n');
    for number in named {
        match links.get(number - 1).and_then(|l| safe_link(l.as_str())) {
            Some(url) => out.push_str(&format!("[{number}] {}\n", url.as_str())),
            None => out.push_str(&format!("[{number}] {NOT_INCLUDED}\n")),
        }
    }
    out
}

/// Every line with `> ` in front (`>` alone before an already quoted or an empty line), ending
/// in a newline; empty text stays empty.
pub fn quoted(text: &str) -> String {
    let mut out = String::new();
    for line in text.lines() {
        if line.is_empty() {
            out.push('>');
        } else if line.starts_with('>') {
            out.push('>');
            out.push_str(line);
        } else {
            out.push_str("> ");
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

/// `body`, then a blank line, `-- ` and the signature's lines, when there is a signature; all
/// with plain line ends.
pub fn with_signature(body: &str, signature: Option<&str>) -> String {
    let body = plain_line_ends(body);
    let Some(signature) = signature.map(|s| s.trim_end_matches(['\n', '\r'])) else {
        return body.into_owned();
    };
    if signature.trim().is_empty() {
        return body.into_owned();
    }
    let signature = plain_line_ends(signature);
    let mut out = body.into_owned();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("\n-- \n");
    out.push_str(&signature);
    out.push('\n');
    out
}

/// `date` RFC 2822 style in `tz`: `Thu, 9 Oct 2025 08:53:20 +0000`.
fn rfc2822(date: i64, tz: &TimeZone) -> String {
    Timestamp::from_second(date).map_or_else(
        |_| "an unknown date".to_string(),
        |t| {
            t.to_zoned(tz.clone())
                .strftime("%a, %-d %b %Y %H:%M:%S %z")
                .to_string()
        },
    )
}

/// Add each mailbox not `me` and not already in `seen` (both compared in lower case).
fn add_unique(out: &mut Vec<String>, seen: &mut Vec<String>, from: &[Mailbox], me: &str) {
    for m in from {
        let key = m.address.to_lowercase();
        if key == me || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        out.push(display(m));
    }
}

/// A reply's headers and body: to the `Reply-To` (else the `From`); with `all`, the original
/// `To` and `Cc` too, minus `me` and duplicates, what was in `Cc` staying in `Cc`. The body
/// leaves two lines to type in above `On <date>, <name> wrote:` and the quoted text.
pub fn reply(source: &ReplySource, all: bool, me: &str, tz: &TimeZone) -> (Headers, String) {
    let targets = if source.reply_to.is_empty() {
        &source.from
    } else {
        &source.reply_to
    };
    let (mut to, mut cc) = (Vec::new(), Vec::new());
    if all {
        let me = me.to_lowercase();
        let mut seen = Vec::new();
        add_unique(&mut to, &mut seen, targets, &me);
        add_unique(&mut to, &mut seen, &source.to, &me);
        add_unique(&mut cc, &mut seen, &source.cc, &me);
    } else {
        to = targets.iter().map(display).collect();
    }
    let who = targets.first().map_or_else(String::new, |m| {
        m.name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map_or_else(|| one_line(&m.address), one_line)
    });
    let body = format!(
        "\n\nOn {}, {who} wrote:\n{}",
        rfc2822(source.date, tz),
        quoted(&with_links(
            source.body_text.as_deref().unwrap_or(""),
            &source.links
        ))
    );
    let headers = Headers {
        to,
        cc,
        bcc: Vec::new(),
        subject: re_subject(&source.subject),
    };
    (headers, body)
}

fn joined(list: &[Mailbox]) -> String {
    list.iter().map(display).collect::<Vec<_>>().join(", ")
}

/// A forward's headers (no recipients) and body: the forwarded-message block, then the
/// original text. Its date is in UTC, with the offset written.
pub fn forward(source: &ReplySource) -> (Headers, String) {
    let mut body = format!(
        "\n\n{FORWARD_BAR}\nFrom: {}\nDate: {}\nSubject: {}\nTo: {}\n",
        joined(&source.from),
        rfc2822(source.date, &TimeZone::UTC),
        one_line(&source.subject),
        joined(&source.to),
    );
    if !source.cc.is_empty() {
        body.push_str(&format!("Cc: {}\n", joined(&source.cc)));
    }
    body.push('\n');
    let text = with_links(source.body_text.as_deref().unwrap_or(""), &source.links);
    body.push_str(&text);
    if !text.is_empty() && !text.ends_with('\n') {
        body.push('\n');
    }
    let headers = Headers {
        subject: fwd_subject(&source.subject),
        ..Headers::default()
    };
    (headers, body)
}
