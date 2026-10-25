//! Yes-or-no confirmations and the link number prompt. The attachment list lives in
//! `modals::attachments`; `human_size` stays here, where the composer's review reads it too.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;
use unicode_segmentation::UnicodeSegmentation;

use super::{Command, ModalOutcome, boxed};
use crate::tui::text::{width, wrap};
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

    /// The body lines (tests).
    #[cfg(test)]
    pub(crate) fn lines(&self) -> &[String] {
        &self.lines
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Char('y' | 'Y') | KeyCode::Enter => ModalOutcome::Run(self.yes.clone()),
            KeyCode::Char('n' | 'N' | 'q') | KeyCode::Esc => ModalOutcome::Close,
            _ => ModalOutcome::Stay,
        }
    }

    /// The box is as tall as the lines wrapped to its width, plus the answer line, at most the
    /// screen. The answer line is always the last row inside the box; when the lines do not fit
    /// above it they are cut there, the last row shown ending in `…`.
    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let cols = usize::from(area.width.min(CONFIRM_WIDTH).saturating_sub(2)).max(1);
        let mut rows: Vec<String> = self.lines.iter().flat_map(|l| wrap(l, cols)).collect();
        let height = u16::try_from(rows.len() + 4).unwrap_or(u16::MAX);
        let inner = boxed(frame, area, CONFIRM_WIDTH, height, &self.title, theme);
        let hint = Rect {
            y: inner.y + inner.height.saturating_sub(1),
            height: inner.height.min(1),
            ..inner
        };
        let above = usize::from(inner.height.saturating_sub(1));
        if rows.len() + 1 > above {
            // No room for the blank separator: cut the lines to what is left.
            let cut = rows.len() > above;
            rows.truncate(above);
            if let (true, Some(last)) = (cut, rows.last_mut()) {
                *last = ellipsise(last, cols);
            }
        }
        let lines: Vec<Line> = rows
            .into_iter()
            .map(|row| Line::styled(row, theme.text))
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
        frame.render_widget(
            Paragraph::new(Line::styled("y yes · n or Esc no", theme.muted)),
            hint,
        );
    }
}

/// `row` ending in `…`, within `cols` columns.
fn ellipsise(row: &str, cols: usize) -> String {
    let mut kept: Vec<&str> = row.graphemes(true).collect();
    while !kept.is_empty() && width(&kept.concat()) + 1 > cols {
        kept.pop();
    }
    kept.push("…");
    kept.concat()
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
