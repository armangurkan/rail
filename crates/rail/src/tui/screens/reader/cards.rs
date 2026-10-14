//! The reader's lines for one width: the title block (the subject, the newest message's
//! sender, time and account, its recipients and the count, a rule), then the newest message,
//! open (its headers when `h` shows them, its text) or collapsed to its preview, and a rule.
//! Every outside string is escaped (`fit`, `escape`, or the body's escaped rows).
#![cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the reader's lines from the wiring task (Task 5) on; remove then"
    )
)]

use rail_core::provider::Mailbox;
use rail_core::store::view::{MessageView, ThreadDetail};

use super::lines::{LineKind, ReaderLine, State, Tone, content, fields, sender};
use crate::tui::text::{escape, fit, width as cells};

/// The lines of `detail` at `width`.
pub(super) fn build(detail: &ThreadDetail, state: &State<'_>, width: usize) -> Vec<ReaderLine> {
    let Some(newest) = detail.messages.len().checked_sub(1) else {
        // A thread whose messages all went: its subject alone.
        let subject = fit(&detail.subject, width);
        return vec![ReaderLine::one(LineKind::Note, subject, Tone::Accent)];
    };
    let mut out = title_block(detail, newest, state, width);
    newest_message(&mut out, detail, newest, state, width);
    out
}

/// The subject (or a failed re-read's error), the sender line, the recipients line and a
/// rule, all on the newest message.
fn title_block(
    detail: &ThreadDetail,
    newest: usize,
    state: &State<'_>,
    width: usize,
) -> Vec<ReaderLine> {
    let kind = LineKind::Title { message: newest };
    let m = &detail.messages[newest];
    let subject = match state.read_error {
        Some(error) => ReaderLine::one(kind, fit(error, width), Tone::Error),
        None => ReaderLine::one(kind, fit(&detail.subject, width), Tone::Accent),
    };
    let count = detail.messages.len() + detail.older_left_out;
    vec![
        subject,
        ReaderLine::of(kind, cut(sender_line(m, state), width)),
        ReaderLine::one(kind, to_line(m, count, width), Tone::Muted),
        rule(state, width),
    ]
}

/// `David Hallas  04:32   A  AXON`: the sender in bold, the time as the list writes it, the
/// account's badge and its label upper-cased; no badge or label for an account Rail no longer
/// has.
fn sender_line(m: &MessageView, state: &State<'_>) -> Vec<(String, Tone)> {
    let mut segments = vec![
        (escape(&sender(m)), Tone::Strong),
        ("  ".to_string(), Tone::Plain),
        (time(m, state), Tone::Muted),
    ];
    let account = state.account;
    if !account.label.is_empty() {
        segments.extend([
            ("  ".to_string(), Tone::Plain),
            (format!(" {} ", account.badge), Tone::Badge),
            (" ".to_string(), Tone::Plain),
            (escape(&account.label.to_uppercase()), Tone::Muted),
        ]);
    }
    segments
}

/// When `m` arrived, as the thread list writes it: `04:32`, `Mon 22:11`, `Oct 03`.
fn time(m: &MessageView, state: &State<'_>) -> String {
    let secs = m.internal_date / 1000;
    state.clock.time_in_group(secs, state.clock.group(secs))
}

/// `To: Vito, dana@example.com  /  4 messages`, the names cut so the count stays whole;
/// `/  4 messages` when the message names no recipient.
fn to_line(m: &MessageView, count: usize, width: usize) -> String {
    let count = match count {
        1 => "1 message".to_string(),
        n => format!("{n} messages"),
    };
    if m.to.is_empty() {
        return fit(&format!("/  {count}"), width);
    }
    let names: Vec<String> = m.to.iter().map(name_of).collect();
    let names = format!("To: {}", names.join(", "));
    let tail = format!("  /  {count}");
    match width.checked_sub(cells(&tail)) {
        Some(room) if room > 0 => format!("{}{tail}", fit(&names, room)),
        _ => fit(&format!("{names}{tail}"), width),
    }
}

/// A mailbox's name, or its address when it has none.
fn name_of(mailbox: &Mailbox) -> String {
    match mailbox.name.as_deref() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => mailbox.address.clone(),
    }
}

/// The width filled with the rule glyph, muted.
fn rule(state: &State<'_>, width: usize) -> ReaderLine {
    ReaderLine::one(LineKind::Rule, state.glyphs.rule.repeat(width), Tone::Muted)
}

fn is_open(state: &State<'_>, index: usize) -> bool {
    state.expanded.get(index).copied().unwrap_or(false)
}

/// The newest message under the title block, open or as its preview, then a rule.
fn newest_message(
    out: &mut Vec<ReaderLine>,
    detail: &ThreadDetail,
    newest: usize,
    state: &State<'_>,
    width: usize,
) {
    if is_open(state, newest) {
        open_message(out, detail, newest, state, width);
    } else {
        let snippet = fit(&detail.messages[newest].snippet, width);
        let kind = LineKind::Preview { message: newest };
        out.push(ReaderLine::one(kind, snippet, Tone::Muted));
    }
    out.push(rule(state, width));
}

/// An open message: its header fields when `h` shows them, then its text.
fn open_message(
    out: &mut Vec<ReaderLine>,
    detail: &ThreadDetail,
    index: usize,
    state: &State<'_>,
    width: usize,
) {
    let m = &detail.messages[index];
    if state.headers {
        fields(out, index, m, detail, state, width);
    }
    content(out, index, m, state, width);
}

/// `segments` cut at `width` columns: the one that crosses the edge is cut with `…`, the rest
/// are dropped.
fn cut(segments: Vec<(String, Tone)>, width: usize) -> Vec<(String, Tone)> {
    let mut out = Vec::new();
    let mut used = 0;
    for (text, tone) in segments {
        let w = cells(&text);
        if used + w <= width {
            used += w;
            out.push((text, tone));
            continue;
        }
        out.push((fit(&text, width - used), tone));
        break;
    }
    out
}
