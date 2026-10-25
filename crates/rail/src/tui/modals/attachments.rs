//! The attachment list the reader's `a` opens, and the part a save or an open names. Rows
//! carry the provenance the two operations need — the account, the provider's message id and
//! the part id — beside what is drawn. Names and types are outside text: they are escaped
//! where they are drawn, and no `Debug` here prints one.

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent};
use rail_core::config::Icons;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::confirm::human_size;
use super::scroll::ScrollBox;
use super::{Command, ModalOutcome, boxed};
use crate::tui::text::{fit, pad, pad_left};
use crate::tui::theme::Theme;

/// The box's width, its border included. `boxed` clamps it to the area, so at both sizes the
/// inner width is 74 and no column moves between them.
pub const ATTACHMENTS_WIDTH: u16 = 76;

/// The keys the last inside row names.
const HINT: &str = "Enter or o opens · s saves · S saves all · j/k move · Esc closes";

/// The cursor's marker column, two cells in both icon sets.
const MARKER_COLS: usize = 2;
/// The name column. Fixed, so a long type or a changing size never moves it.
const NAME_COLS: usize = 38;
/// The type column.
const TYPE_COLS: usize = 24;
/// The size column, right-aligned, so `512 B`, `2.0 KB` and `1.5 MB` all end in the same
/// place.
const SIZE_COLS: usize = 8;
/// Marker, name, a space, type, a space and size: the inner width at both sizes.
const ROW_COLS: usize = MARKER_COLS + NAME_COLS + 1 + TYPE_COLS + 1 + SIZE_COLS;

/// One attachment as listed, with the provenance `attachment.save` and `attachment.open`
/// need. `name` and `mime_type` are the sender's text.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentRow {
    /// The account's name, as `config.toml` writes it.
    pub account: String,
    /// The provider's id for the message this part belongs to.
    pub message: String,
    /// The provider's id for the part itself.
    pub part_id: String,
    pub name: String,
    pub mime_type: String,
    pub size: i64,
    /// The message's position in `ThreadDetail.messages`, zero-based; the `MESSAGE <n>`
    /// heading shows it one-based.
    pub message_index: usize,
}

/// The part a `Command` names, with what the status line has to say about it.
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentTarget {
    pub account: String,
    pub message: String,
    pub part_id: String,
    pub filename: String,
    pub mime_type: String,
}

/// One row the box draws: a message's heading, or one of its parts.
enum Entry {
    /// `MESSAGE <n>`, `n` the message's one-based position in the thread.
    Heading(usize),
    /// The part at this index in `rows`.
    Part(usize),
}

pub struct AttachmentList {
    rows: Vec<AttachmentRow>,
    /// The highlighted part: an index into `rows`, so `j` and `k` step over the headings.
    cursor: usize,
    /// More than one message holds attachments, so each message's rows take a heading.
    multi: bool,
    icons: Icons,
    scroll: ScrollBox,
}

impl AttachmentList {
    #[must_use]
    pub fn new(rows: Vec<AttachmentRow>, icons: Icons) -> Self {
        let first = rows.first().map(|row| row.message_index);
        let multi = rows.iter().any(|row| Some(row.message_index) != first);
        Self {
            rows,
            cursor: 0,
            multi,
            icons,
            scroll: ScrollBox::new(icons),
        }
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        let last = self.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => self.cursor = (self.cursor + 1).min(last),
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Enter | KeyCode::Char('o') => return self.run(Command::OpenAttachment),
            KeyCode::Char('s') => return self.run(Command::SaveAttachment),
            KeyCode::Char('S') => return self.run(Command::SaveAllAttachments),
            KeyCode::Esc | KeyCode::Char('q' | 'a') => return ModalOutcome::Close,
            _ => {}
        }
        ModalOutcome::Stay
    }

    /// `make`'s command for the highlighted part. An empty list is never drawn (`a` notifies
    /// instead), so there is nothing to ask for.
    fn run(&self, make: fn(AttachmentTarget) -> Command) -> ModalOutcome {
        let Some(row) = self.rows.get(self.cursor) else {
            return ModalOutcome::Stay;
        };
        ModalOutcome::Run(make(AttachmentTarget {
            account: row.account.clone(),
            message: row.message.clone(),
            part_id: row.part_id.clone(),
            filename: row.name.clone(),
            mime_type: row.mime_type.clone(),
        }))
    }

    /// Every drawn row: each message's heading (when more than one has attachments) followed
    /// by its parts, in the order the rows came in.
    fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::with_capacity(self.rows.len() * 2);
        let mut message = None;
        for (index, row) in self.rows.iter().enumerate() {
            if self.multi && message != Some(row.message_index) {
                out.push(Entry::Heading(row.message_index + 1));
                message = Some(row.message_index);
            }
            out.push(Entry::Part(index));
        }
        out
    }

    /// One entry as a line. A heading starts in the marker column, upper-cased and muted, so
    /// it is told apart by position as well as by colour; a part row keeps the fixed budget.
    fn line(&self, entry: &Entry, theme: &Theme) -> Line<'static> {
        match entry {
            Entry::Heading(number) => Line::styled(
                pad(&format!("MESSAGE {number}"), ROW_COLS),
                theme.muted.add_modifier(Modifier::BOLD),
            ),
            Entry::Part(index) => {
                let row = &self.rows[*index];
                let marker = if *index == self.cursor {
                    self.marker()
                } else {
                    " ".repeat(MARKER_COLS)
                };
                let style = if *index == self.cursor {
                    theme.selected
                } else {
                    theme.text
                };
                let text = format!(
                    "{marker}{} {} {}",
                    pad(&row.name, NAME_COLS),
                    pad(&row.mime_type, TYPE_COLS),
                    pad_left(&human_size(row.size), SIZE_COLS)
                );
                Line::styled(text, style)
            }
        }
    }

    /// The cursor's own glyph: `▸ ` where the terminal draws Unicode, `> ` where it does not.
    fn marker(&self) -> String {
        match self.icons {
            Icons::Unicode => "▸ ".to_string(),
            Icons::Ascii => "> ".to_string(),
        }
    }

    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let entries = self.entries();
        let height = u16::try_from(entries.len())
            .unwrap_or(u16::MAX)
            .saturating_add(4);
        let inner = boxed(frame, area, ATTACHMENTS_WIDTH, height, "Attachments", theme);
        if inner.height == 0 {
            return;
        }
        // The hint goes below inner height 2, the blank separator below 3; the body loses its
        // rows from the bottom and `ScrollBox` says when rows are hidden.
        let hint = inner.height >= 2;
        let blank = inner.height >= 3;
        let body = Rect {
            height: inner
                .height
                .saturating_sub(u16::from(hint))
                .saturating_sub(u16::from(blank)),
            ..inner
        };
        let lines: Vec<Line<'static>> = entries.iter().map(|e| self.line(e, theme)).collect();
        let at = entries
            .iter()
            .position(|e| matches!(e, Entry::Part(index) if *index == self.cursor))
            .unwrap_or(0);
        self.scroll
            .render_revealing(frame, body, lines, theme, at, at);
        if !hint {
            return;
        }
        let row = Rect {
            y: inner.y + inner.height - 1,
            height: 1,
            ..inner
        };
        let text = fit(HINT, usize::from(inner.width));
        frame.render_widget(Paragraph::new(Line::styled(text, theme.muted)), row);
    }
}

/// Numbers only: a row names an account, a message, a part, a file and a type, all of which
/// are mail.
impl fmt::Debug for AttachmentRow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentRow")
            .field("message_index", &self.message_index)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

/// Nothing: every field is mail.
impl fmt::Debug for AttachmentTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttachmentTarget").finish_non_exhaustive()
    }
}
