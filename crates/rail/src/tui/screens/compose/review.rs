//! The review screen's rows: the header block, any refusal, the attachments with their total,
//! then the body with long quoted runs folded. Every outside string is escaped here.

use rail_core::store::drafts::DraftKind;

use super::RowKind;
use super::model::Composing;
use crate::tui::modals::confirm::human_size;
use crate::tui::text::{Block, blocks, escape, expand_tabs, fold_label, shown_host, wrap};

/// The bottom row: the review's keys (the default ones; Task 7 builds it from the keymap).
pub(super) const HINT: &str =
    "y send · e edit · t headers · a attach · x remove · q keep · d discard";
/// The bottom row while the discard question is open.
pub(super) const DISCARD_QUESTION: &str = "Discard this draft? (y/n)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowStyle {
    Text,
    Muted,
    Error,
}

/// One row of the review, its text already escaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Row {
    pub text: String,
    pub kind: RowKind,
    pub style: RowStyle,
}

impl Row {
    fn new(text: String, kind: RowKind, style: RowStyle) -> Self {
        Self { text, kind, style }
    }
}

/// `Reply · me@example.com`, escaped.
pub(super) fn title(c: &Composing) -> String {
    let kind = match c.kind {
        DraftKind::New => "New message",
        DraftKind::Reply => "Reply",
        DraftKind::ReplyAll => "Reply all",
        DraftKind::Forward => "Forward",
    };
    format!("{kind} · {}", escape(&c.account_address))
}

/// The domain of a recipient as typed (`Name <a@b>` or `a@b`).
fn domain_of(recipient: &str) -> Option<&str> {
    let address = match (recipient.rfind('<'), recipient.rfind('>')) {
        (Some(open), Some(close)) if close > open => &recipient[open + 1..close],
        _ => recipient,
    };
    address.rsplit_once('@').map(|(_, domain)| domain.trim())
}

/// A recipient escaped and, when its domain is international, followed by its ASCII form.
fn recipient(text: &str) -> String {
    let shown = escape(text.trim());
    let ascii = domain_of(text)
        .filter(|d| !d.is_ascii())
        .and_then(shown_host);
    match ascii {
        Some(host) => format!("{shown} ({host})"),
        None => shown,
    }
}

fn push_wrapped(out: &mut Vec<Row>, text: &str, width: usize, kind: RowKind, style: RowStyle) {
    out.extend(
        wrap(text, width)
            .into_iter()
            .map(|t| Row::new(t, kind, style)),
    );
}

fn header_rows(c: &Composing, width: usize, out: &mut Vec<Row>) {
    let h = &c.headers;
    let mut fields = vec![("From", escape(&c.account_address))];
    for (label, list, always) in [
        ("To", &h.to, true),
        ("Cc", &h.cc, false),
        ("Bcc", &h.bcc, false),
    ] {
        if always || !list.is_empty() {
            let shown: Vec<String> = list.iter().map(|r| recipient(r)).collect();
            fields.push((label, shown.join(", ")));
        }
    }
    fields.push(("Subject", escape(&h.subject)));
    for (label, value) in fields {
        let text = format!("{label}: {value}");
        push_wrapped(out, &text, width, RowKind::Header, RowStyle::Text);
    }
    for error in &c.errors {
        push_wrapped(out, &escape(error), width, RowKind::Header, RowStyle::Error);
    }
}

fn attachment_rows(c: &Composing, glyph: &str, out: &mut Vec<Row>) {
    let count = c.attachments.len();
    if count == 0 {
        return;
    }
    let noun = if count == 1 {
        "attachment"
    } else {
        "attachments"
    };
    let total = human_size(c.total_size());
    let summary = match glyph.trim_end() {
        "" => format!("{count} {noun}, {total}"),
        mark => format!("{mark} {count} {noun}, {total}"),
    };
    out.push(Row::new(summary, RowKind::Header, RowStyle::Muted));
    for (index, a) in c.attachments.iter().enumerate() {
        let text = format!(
            "  {}  {}  {}",
            escape(&a.name),
            escape(&a.mime),
            human_size(a.size)
        );
        out.push(Row::new(text, RowKind::Attachment(index), RowStyle::Text));
    }
}

fn body_rows(body: &str, width: usize, out: &mut Vec<Row>) {
    if body.trim().is_empty() {
        out.push(Row::new("(no text)".into(), RowKind::Body, RowStyle::Muted));
        return;
    }
    for block in blocks(body) {
        match block {
            Block::Line(line) => {
                let text = escape(&expand_tabs(line));
                push_wrapped(out, &text, width, RowKind::Body, RowStyle::Text);
            }
            Block::Quote(run) => {
                out.push(Row::new(
                    fold_label(run.len()),
                    RowKind::Body,
                    RowStyle::Muted,
                ));
            }
        }
    }
}

/// Every row of the review at `width` columns; `glyph` marks the attachment summary.
pub(super) fn rows(c: &Composing, width: usize, glyph: &str) -> Vec<Row> {
    let width = width.max(1);
    let mut out = Vec::new();
    header_rows(c, width, &mut out);
    attachment_rows(c, glyph, &mut out);
    out.push(Row::new(String::new(), RowKind::Body, RowStyle::Text));
    body_rows(&c.body, width, &mut out);
    out
}
