//! The reader's lines for one width: the title block (the subject, the newest message's
//! sender, time and account, its recipients and the count, a rule), the newest message, open
//! (its headers when `h` shows them, its text) or collapsed to its preview, and a rule; then
//! each older message as a card (its number, sender and time over a preview, or the message
//! itself), oldest or newest first, with a rule after each, and the note for the messages left
//! out where they would be. Every outside string is escaped (`fit`, `escape`, or the body's
//! escaped rows).

use rail_core::config::ThreadOrder;
use rail_core::store::view::{MessageView, ThreadDetail};

use super::lines::{LineKind, ReaderLine, State, Tone, content, display_name, fields, sender};
use crate::tui::clock::Group;
use crate::tui::text::{escape, fit, width as cells};

/// The cells a card keeps for its sender however narrow the pane: the time is cut first.
const MIN_SENDER: usize = 3;
/// The least room between a card's sender and its time.
const TIME_GAP: usize = 2;
/// The least room the title's recipients get: under it they are dropped and the count stands
/// alone.
const MIN_NAMES: usize = 4;

/// The lines of `detail` at `width`.
pub(super) fn build(detail: &ThreadDetail, state: &State<'_>, width: usize) -> Vec<ReaderLine> {
    let Some(newest) = detail.messages.len().checked_sub(1) else {
        // A thread whose messages all went: its subject alone.
        let subject = fit(&detail.subject, width);
        return vec![ReaderLine::one(LineKind::Note, subject, Tone::Accent)];
    };
    let mut out = title_block(detail, newest, state, width);
    newest_message(&mut out, detail, newest, state, width);
    let older: Vec<usize> = match state.order {
        ThreadOrder::Oldest => (0..newest).collect(),
        ThreadOrder::Newest => (0..newest).rev().collect(),
    };
    // The messages left out are older than every card: before them oldest first, after them
    // newest first.
    let (before, after) = match state.order {
        ThreadOrder::Oldest => (left_out(detail, width), None),
        ThreadOrder::Newest => (None, left_out(detail, width)),
    };
    if let Some(note) = before {
        out.push(note);
        out.push(rule(state, width));
    }
    for index in older {
        card(&mut out, detail, index, state, width);
    }
    out.extend(after);
    out
}

/// `[5 older messages of this thread are not shown]`, when some are.
fn left_out(detail: &ThreadDetail, width: usize) -> Option<ReaderLine> {
    (detail.older_left_out > 0).then(|| {
        let note = format!(
            "[{} older messages of this thread are not shown]",
            detail.older_left_out
        );
        ReaderLine::one(LineKind::Note, fit(&note, width), Tone::Muted)
    })
}

/// The subject (or a failed re-read's error), the sender line, the recipients line, until when
/// the thread is snoozed (while it is) and a rule, all on the newest message.
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
    let snoozed = detail.snoozed_until.map(|until| {
        let text = format!("Snoozed until {}", state.clock.long(until));
        ReaderLine::one(kind, fit(&text, width), Tone::Muted)
    });
    let mut lines = vec![
        subject,
        ReaderLine::of(kind, cut(sender_line(m, state), width)),
        ReaderLine::one(kind, to_line(m, count, width), Tone::Muted),
    ];
    lines.extend(snoozed);
    lines.push(rule(state, width));
    lines
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

/// When `m` arrived, as the thread list writes it (`04:32`, `Mon 22:11`, `Oct 03`), except
/// that yesterday takes the week form, `Thu 08:55`: the reader has no heading to tell the days
/// apart.
fn time(m: &MessageView, state: &State<'_>) -> String {
    let secs = m.internal_date / 1000;
    let group = match state.clock.group(secs) {
        Group::Yesterday => Group::ThisWeek,
        group => group,
    };
    state.clock.time_in_group(secs, group)
}

/// `To: Vito, dana@example.com  /  4 messages`, the names cut so the count stays whole;
/// `/  4 messages` when the message names no recipient or under `MIN_NAMES` cells are left
/// for them.
fn to_line(m: &MessageView, count: usize, width: usize) -> String {
    let count = match count {
        1 => "1 message".to_string(),
        n => format!("{n} messages"),
    };
    let alone = format!("/  {count}");
    if m.to.is_empty() {
        return fit(&alone, width);
    }
    let names: Vec<String> = m.to.iter().map(display_name).collect();
    let names = format!("To: {}", names.join(", "));
    let tail = format!("  /  {count}");
    match width.checked_sub(cells(&tail)) {
        Some(room) if room >= MIN_NAMES => format!("{}{tail}", fit(&names, room)),
        Some(_) => fit(&alone, width),
        None => fit(&format!("{names}{tail}"), width),
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

/// An older message: its card line, then its preview under the sender or, open, the message
/// itself; then a rule.
fn card(
    out: &mut Vec<ReaderLine>,
    detail: &ThreadDetail,
    index: usize,
    state: &State<'_>,
    width: usize,
) {
    let m = &detail.messages[index];
    let open = is_open(state, index);
    let prefix = card_prefix(detail.older_left_out + index + 1, open, state);
    let indent = segments_width(&prefix).min(width);
    let line = card_line(m, prefix, state, width);
    out.push(ReaderLine::of(LineKind::Card { message: index }, line));
    if open {
        open_message(out, detail, index, state, width);
    } else {
        let snippet = fit(&m.snippet, width - indent);
        let preview = format!("{}{snippet}", " ".repeat(indent));
        out.push(ReaderLine::one(
            LineKind::Preview { message: index },
            preview,
            Tone::Muted,
        ));
    }
    out.push(rule(state, width));
}

/// `▸   2   `: the arrow (`▾` while open), two spaces, the number in its box (counted from the
/// thread's oldest message), two spaces.
fn card_prefix(number: usize, open: bool, state: &State<'_>) -> Vec<(String, Tone)> {
    let arrow = if open {
        state.glyphs.open
    } else {
        state.glyphs.closed
    };
    vec![
        (arrow.to_string(), Tone::Muted),
        ("  ".to_string(), Tone::Plain),
        (format!(" {number} "), Tone::Number),
        ("  ".to_string(), Tone::Plain),
    ]
}

/// `segments` (the prefix), the sender (bold while unread, ` ★` when starred) cut so the time
/// fits whole at the right edge; below about 20 columns the sender keeps three cells and the
/// time is cut at the edge.
fn card_line(
    m: &MessageView,
    mut segments: Vec<(String, Tone)>,
    state: &State<'_>,
    width: usize,
) -> Vec<(String, Tone)> {
    let star = if m.is_starred {
        format!(" {}", state.glyphs.star)
    } else {
        String::new()
    };
    // Opening the thread marks it read, so the sender is bold by what was unread at open.
    let tone = if state.unread.contains(&m.id) {
        Tone::Strong
    } else {
        Tone::Plain
    };
    let time = time(m, state);
    let used = segments_width(&segments);
    let room = width
        .saturating_sub(used + TIME_GAP + cells(&time))
        .max(MIN_SENDER);
    // The name is cut, the star kept whole.
    let name_room = room.saturating_sub(cells(&star)).max(1);
    let name = format!("{}{star}", fit(&sender(m), name_room));
    let pad = width
        .saturating_sub(used + cells(&name) + cells(&time))
        .max(1);
    segments.extend([
        (name, tone),
        (" ".repeat(pad), Tone::Plain),
        (time, Tone::Muted),
    ]);
    cut(segments, width)
}

fn segments_width(segments: &[(String, Tone)]) -> usize {
    segments.iter().map(|(text, _)| cells(text)).sum()
}

/// `segments` cut at `width` columns: the one that crosses the edge is cut with `…`, the rest
/// are dropped; when the row is full exactly at a segment's end and more follow, that last kept
/// segment shows the `…`.
pub(super) fn cut(segments: Vec<(String, Tone)>, width: usize) -> Vec<(String, Tone)> {
    let mut out: Vec<(String, Tone)> = Vec::new();
    let mut used = 0;
    for (text, tone) in segments {
        let w = cells(&text);
        if w == 0 && used == width {
            continue;
        }
        if used + w <= width {
            used += w;
            out.push((text, tone));
            continue;
        }
        if used < width {
            out.push((fit(&text, width - used), tone));
        } else if let Some((last, _)) = out.last_mut() {
            *last = fit(&format!("{last}…"), cells(last));
        }
        break;
    }
    out
}
