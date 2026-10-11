//! Yes-or-no confirmations, the link number prompt, and the read-only attachment list.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::{Command, ModalOutcome, boxed};
use crate::tui::text::{fit, pad, wrap};
use crate::tui::theme::Theme;

/// A confirmation's width, its border included.
const CONFIRM_WIDTH: u16 = 72;

/// Asks before doing `yes`. `lines` are already escaped.
pub struct Confirm {
    title: String,
    lines: Vec<String>,
    yes: Command,
}

impl Confirm {
    pub fn new(title: &str, lines: Vec<String>, yes: Command) -> Self {
        Self {
            title: title.to_string(),
            lines,
            yes,
        }
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => ModalOutcome::Run(self.yes.clone()),
            KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => ModalOutcome::Close,
            _ => ModalOutcome::Stay,
        }
    }

    /// The box is as tall as the lines wrapped to its width, plus the answer line.
    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let cols = usize::from(area.width.min(CONFIRM_WIDTH).saturating_sub(2)).max(1);
        let rows: Vec<String> = self.lines.iter().flat_map(|l| wrap(l, cols)).collect();
        let height = u16::try_from(rows.len() + 4).unwrap_or(u16::MAX);
        let inner = boxed(frame, area, CONFIRM_WIDTH, height, &self.title, theme);
        let mut lines: Vec<Line> = rows
            .into_iter()
            .map(|row| Line::styled(row, theme.text))
            .collect();
        lines.push(Line::default());
        lines.push(Line::styled("y yes · n or Esc no", theme.muted));
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

/// Asks which of `links` to open, by number.
pub struct LinkPrompt {
    links: Vec<String>,
    typed: String,
}

impl LinkPrompt {
    pub fn new(links: Vec<String>) -> Self {
        Self {
            links,
            typed: String::new(),
        }
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Char(c) if c.is_ascii_digit() && self.typed.len() < 3 => {
                self.typed.push(c);
                ModalOutcome::Stay
            }
            KeyCode::Backspace => {
                self.typed.pop();
                ModalOutcome::Stay
            }
            KeyCode::Enter => match self.typed.parse::<usize>() {
                Ok(number) if (1..=self.links.len()).contains(&number) => {
                    ModalOutcome::Run(Command::ChooseLink {
                        links: self.links.clone(),
                        number,
                    })
                }
                _ => ModalOutcome::Stay,
            },
            KeyCode::Esc => ModalOutcome::Close,
            _ => ModalOutcome::Stay,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let inner = boxed(frame, area, 48, 4, "Open a link", theme);
        let lines = vec![
            Line::styled(
                format!("Open link number (1-{}): {}▏", self.links.len(), self.typed),
                theme.text,
            ),
            Line::styled("Enter open · Esc cancel", theme.muted),
        ];
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

/// One attachment as listed: name and type are outside text, escaped when drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRow {
    pub name: String,
    pub mime_type: String,
    pub size: i64,
}

pub struct AttachmentList {
    rows: Vec<AttachmentRow>,
}

impl AttachmentList {
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
        let inner = boxed(frame, area, 76, height, "Attachments", theme);
        let cols = usize::from(inner.width);
        let mut lines: Vec<Line> = self
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
            fit(
                "Rail lists attachments; opening and saving them comes in a later version.",
                cols,
            ),
            theme.muted,
        ));
        frame.render_widget(Paragraph::new(lines), inner);
    }
}

/// `512 B`, `2.0 KB`, `1.5 MB`, `3.2 GB`.
pub fn human_size(bytes: i64) -> String {
    let bytes = bytes.max(0) as f64;
    let units = ["KB", "MB", "GB"];
    if bytes < 1024.0 {
        return format!("{bytes} B");
    }
    let mut value = bytes / 1024.0;
    for unit in units {
        if value < 1024.0 || unit == "GB" {
            return format!("{value:.1} {unit}");
        }
        value /= 1024.0;
    }
    format!("{value:.1} GB")
}
