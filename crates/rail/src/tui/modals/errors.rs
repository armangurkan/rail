//! `:errors`: the session's recent errors, newest first (their text already escaped),
//! wrapped to the box and scrolled with the keys help uses.

use crossterm::event::{KeyCode, KeyEvent};
use rail_core::config::Icons;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::scroll::ScrollBox;
use super::{ModalOutcome, boxed};
use crate::tui::text::wrap;
use crate::tui::theme::Theme;

pub struct ErrorList {
    rows: Vec<String>,
    scroll: ScrollBox,
}

impl ErrorList {
    pub fn new(rows: Vec<String>, icons: Icons) -> Self {
        Self {
            rows,
            scroll: ScrollBox::new(icons),
        }
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Enter | KeyCode::Char('q') => ModalOutcome::Close,
            _ => {
                self.scroll.key(key);
                ModalOutcome::Stay
            }
        }
    }

    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let height = area.height.saturating_sub(4);
        let inner = boxed(frame, area, 86, height, "Recent errors", theme);
        if self.rows.is_empty() {
            let none = Line::styled("No errors this session.", theme.muted);
            frame.render_widget(Paragraph::new(none), inner);
            return;
        }
        let cols = usize::from(inner.width);
        let lines = self
            .rows
            .iter()
            .flat_map(|row| wrap(row, cols))
            .map(|row| Line::styled(row, theme.text))
            .collect();
        self.scroll.render(frame, inner, lines, theme);
    }
}
