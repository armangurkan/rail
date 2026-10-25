//! The attachment list the reader's `a` opens, and the part a save or an open names. Rows
//! carry the provenance the two operations need — the account, the provider's message id and
//! the part id — beside what is drawn. Names and types are outside text: they are escaped
//! where they are drawn, and no `Debug` here prints one.

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::confirm::human_size;
use super::{ModalOutcome, boxed};
use crate::tui::text::{fit, pad};
use crate::tui::theme::Theme;

/// The box's width, its border included. `boxed` clamps it to the area, so at both sizes the
/// inner width is 74 and no column moves between them.
pub const ATTACHMENTS_WIDTH: u16 = 76;

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
// The modal's own keys build it in the modal step; AC-32 already pins its `Debug`. `allow`
// rather than `expect`: rustc 1.88 (the MSRV) does not call this struct dead while 1.99 does,
// so an expectation would be unfulfilled on one of the two toolchains CI checks.
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "built by the modal's save and open keys, a later step"
    )
)]
#[derive(Clone, PartialEq, Eq)]
pub struct AttachmentTarget {
    pub account: String,
    pub message: String,
    pub part_id: String,
    pub filename: String,
    pub mime_type: String,
}

pub struct AttachmentList {
    rows: Vec<AttachmentRow>,
}

impl AttachmentList {
    #[must_use]
    pub fn new(rows: Vec<AttachmentRow>) -> Self {
        Self { rows }
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q' | 'a') => ModalOutcome::Close,
            _ => ModalOutcome::Stay,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let height = u16::try_from(self.rows.len())
            .unwrap_or(u16::MAX)
            .saturating_add(4);
        let inner = boxed(frame, area, ATTACHMENTS_WIDTH, height, "Attachments", theme);
        let cols = usize::from(inner.width);
        let mut lines: Vec<Line<'_>> = self
            .rows
            .iter()
            .map(|row| {
                let size = human_size(row.size);
                let name_cols = cols.saturating_sub(30 + size.len());
                Line::styled(
                    format!(
                        "{} {} {size}",
                        pad(&row.name, name_cols),
                        pad(&row.mime_type, 28)
                    ),
                    theme.text,
                )
            })
            .collect();
        lines.push(Line::default());
        lines.push(Line::styled(
            fit("Rail lists this thread's attachments.", cols),
            theme.muted,
        ));
        frame.render_widget(Paragraph::new(lines), inner);
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
