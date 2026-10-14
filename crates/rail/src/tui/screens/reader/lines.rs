//! The reader's line types and the parts of an open message at one width: its header fields,
//! its body (wrapped, quote runs folded, link markers as written) and its attachments line;
//! `cards.rs` puts the thread together. Every outside string is escaped (by `fit`, `wrap` of
//! escaped text, or `escape`).

use std::collections::{HashMap, HashSet};

use rail_core::config::ThreadOrder;
use rail_core::provider::Mailbox;
use rail_core::store::view::{BodyState, MessageView, ThreadDetail};

use super::ReaderAccount;
use crate::tui::clock::Clock;
use crate::tui::screens::mail::Glyphs;
use crate::tui::text::{Block, blocks, escape, expand_tabs, fit, fold_label, wrap};

/// What a line belongs to, for the cursor's keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A thread-wide note: the left-out count, or an empty thread's subject.
    Note,
    /// The title block's subject, sender and recipients lines, on the newest message.
    Title {
        message: usize,
    },
    /// An older message's card line.
    Card {
        message: usize,
    },
    /// A collapsed message's preview.
    Preview {
        message: usize,
    },
    Field {
        message: usize,
    },
    Body {
        message: usize,
    },
    Fold {
        message: usize,
        block: usize,
    },
    Quote {
        message: usize,
        block: usize,
    },
    Status {
        message: usize,
    },
    /// A rule between the title block, the newest message and the cards.
    Rule,
    Gap,
}

impl LineKind {
    pub fn message(self) -> Option<usize> {
        match self {
            Self::Title { message }
            | Self::Card { message }
            | Self::Preview { message }
            | Self::Field { message }
            | Self::Body { message }
            | Self::Fold { message, .. }
            | Self::Quote { message, .. }
            | Self::Status { message } => Some(message),
            Self::Note | Self::Rule | Self::Gap => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Plain,
    Strong,
    Muted,
    Quote,
    Error,
    /// The subject: the accent colour, bold.
    Accent,
    /// The account's badge: black on the account's colour.
    Badge,
    /// A card's number: the text colour on the border colour.
    Number,
}

/// One line, escaped and at most the width it was built for: segments, each drawn in its tone,
/// and `text`, their concatenation (what `fit`, `markers` and the tests read). It holds mail
/// text, so it is never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct ReaderLine {
    pub kind: LineKind,
    pub text: String,
    pub segments: Vec<(String, Tone)>,
}

impl ReaderLine {
    /// A line in one tone.
    pub fn one(kind: LineKind, text: String, tone: Tone) -> Self {
        Self {
            kind,
            segments: vec![(text.clone(), tone)],
            text,
        }
    }

    /// A line of segments, the empty ones dropped; `text` is their concatenation.
    pub fn of(kind: LineKind, segments: Vec<(String, Tone)>) -> Self {
        let segments: Vec<(String, Tone)> = segments
            .into_iter()
            .filter(|(text, _)| !text.is_empty())
            .collect();
        let text = segments.iter().map(|(text, _)| text.as_str()).collect();
        Self {
            kind,
            text,
            segments,
        }
    }
}

impl std::fmt::Debug for ReaderLine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let tones: Vec<Tone> = self.segments.iter().map(|(_, tone)| *tone).collect();
        f.debug_struct("ReaderLine")
            .field("kind", &self.kind)
            .field("tones", &tones)
            .finish_non_exhaustive()
    }
}

/// What the lines depend on besides the thread.
pub(super) struct State<'a> {
    pub expanded: &'a [bool],
    pub unfolded: &'a HashSet<(usize, usize)>,
    pub fetching: &'a HashSet<i64>,
    pub errors: &'a HashMap<i64, String>,
    pub clock: &'a Clock,
    pub glyphs: &'a Glyphs,
    /// The thread's account, for the title block's badge and label.
    pub account: &'a ReaderAccount,
    /// `h`: every open message shows its header fields.
    pub headers: bool,
    /// How the older messages run.
    pub order: ThreadOrder,
    /// A failed re-read's error (escaped), drawn in place of the subject.
    pub read_error: Option<&'a str>,
}

fn line(kind: LineKind, text: String, tone: Tone) -> ReaderLine {
    ReaderLine::one(kind, text, tone)
}

/// `Name <address>` (or the address) for each mailbox, joined.
fn mailboxes(people: &[Mailbox]) -> String {
    people
        .iter()
        .map(|p| match p.name.as_deref() {
            Some(name) if !name.is_empty() => format!("{name} <{}>", p.address),
            _ => p.address.clone(),
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(super) fn sender(message: &MessageView) -> String {
    match message.from.first() {
        Some(Mailbox {
            name: Some(name), ..
        }) if !name.is_empty() => name.clone(),
        Some(first) => first.address.clone(),
        None => "(no sender)".to_string(),
    }
}

/// An open message's header fields: `From:`, `To:` and `Cc:` (each when not empty), `Date:`,
/// and `Subject:` when it differs from the thread's; muted, cut to the width.
pub(super) fn fields(
    out: &mut Vec<ReaderLine>,
    index: usize,
    m: &MessageView,
    detail: &ThreadDetail,
    state: &State<'_>,
    width: usize,
) {
    let field = |text: String| {
        line(
            LineKind::Field { message: index },
            fit(&text, width),
            Tone::Muted,
        )
    };
    for (label, people) in [("From", &m.from), ("To", &m.to), ("Cc", &m.cc)] {
        if !people.is_empty() {
            out.push(field(format!("{label}: {}", mailboxes(people))));
        }
    }
    out.push(field(format!("Date: {}", state.clock.long(m.date))));
    if m.subject != detail.subject {
        out.push(field(format!("Subject: {}", m.subject)));
    }
}

/// An open message's text: a gap, the body, the attachments line, a gap.
pub(super) fn content(
    out: &mut Vec<ReaderLine>,
    index: usize,
    m: &MessageView,
    state: &State<'_>,
    width: usize,
) {
    out.push(line(LineKind::Gap, String::new(), Tone::Plain));
    body(out, index, m, state, width);
    if !m.attachments.is_empty() {
        let names: Vec<&str> = m.attachments.iter().map(|a| a.filename.as_str()).collect();
        let text = format!(
            "{} {}",
            state.glyphs.attachment.trim_end(),
            names.join(", ")
        );
        out.push(line(
            LineKind::Field { message: index },
            fit(&text, width),
            Tone::Muted,
        ));
    }
    out.push(line(LineKind::Gap, String::new(), Tone::Plain));
}

fn status(out: &mut Vec<ReaderLine>, index: usize, text: &str, tone: Tone, width: usize) {
    for row in wrap(text, width) {
        out.push(line(LineKind::Status { message: index }, row, tone));
    }
}

fn body(out: &mut Vec<ReaderLine>, index: usize, m: &MessageView, state: &State<'_>, width: usize) {
    if let Some(error) = state.errors.get(&m.id) {
        status(out, index, error, Tone::Error, width);
        return;
    }
    match &m.body {
        BodyState::NotFetched if state.fetching.contains(&m.id) => {
            status(out, index, "fetching…", Tone::Muted, width);
        }
        BodyState::NotFetched => status(out, index, "Not fetched yet.", Tone::Muted, width),
        BodyState::Unreadable => {
            status(
                out,
                index,
                &escape(rail_core::sync::UNREADABLE_BODY),
                Tone::Muted,
                width,
            );
        }
        BodyState::Text { text, cut } => {
            text_lines(out, index, text, state, width);
            if *cut {
                let note = "[Rail shows the first 500,000 characters of this message.]";
                status(out, index, note, Tone::Muted, width);
            }
        }
    }
}

fn text_lines(
    out: &mut Vec<ReaderLine>,
    index: usize,
    text: &str,
    state: &State<'_>,
    width: usize,
) {
    for (block_index, block) in blocks(text).into_iter().enumerate() {
        match block {
            Block::Line(raw) => {
                for row in wrap(&escape(&expand_tabs(raw)), width) {
                    out.push(line(LineKind::Body { message: index }, row, Tone::Plain));
                }
            }
            Block::Quote(rows) if state.unfolded.contains(&(index, block_index)) => {
                for raw in rows {
                    for row in wrap(&escape(&expand_tabs(raw)), width) {
                        let kind = LineKind::Quote {
                            message: index,
                            block: block_index,
                        };
                        out.push(line(kind, row, Tone::Quote));
                    }
                }
            }
            Block::Quote(rows) => {
                let kind = LineKind::Fold {
                    message: index,
                    block: block_index,
                };
                out.push(line(kind, fit(&fold_label(rows.len()), width), Tone::Muted));
            }
        }
    }
}
