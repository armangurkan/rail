//! `:errors`: the session's recent errors, newest first (their text already escaped).

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Wrap};

use super::{ModalOutcome, boxed};
use crate::tui::theme::Theme;

pub struct ErrorList {
    rows: Vec<String>,
}

impl ErrorList {
    pub fn new(rows: Vec<String>) -> Self {
        Self { rows }
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => ModalOutcome::Close,
            _ => ModalOutcome::Stay,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let height = area.height.saturating_sub(4);
        let inner = boxed(frame, area, 86, height, "Recent errors", theme);
        let lines: Vec<Line> = if self.rows.is_empty() {
            vec![Line::styled("No errors this session.", theme.muted)]
        } else {
            self.rows
                .iter()
                .map(|r| Line::styled(r.clone(), theme.text))
                .collect()
        };
        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}
