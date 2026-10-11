//! `?`: the keys of the active view and the global ones, from the live keymap, with the keys
//! set in `config.toml` marked.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::{ModalOutcome, boxed};
use crate::tui::keys::{Keymap, View};
use crate::tui::text::{escape, fit, pad};
use crate::tui::theme::Theme;

/// Columns of the keys column.
const KEY_COLS: usize = 20;

pub struct Help {
    /// (heading?, text) rows: headings are styled apart.
    rows: Vec<(bool, String)>,
    offset: usize,
}

impl Help {
    pub fn new(keymap: &Keymap, views: &[View]) -> Self {
        let mut rows = Vec::new();
        let mut marked = false;
        for &view in views {
            rows.push((true, view.title().to_string()));
            for row in keymap.help(view) {
                let mark = if row.overridden { " *" } else { "" };
                marked |= row.overridden;
                let keys = pad(&format!("{}{mark}", escape(&row.keys)), KEY_COLS);
                rows.push((false, format!("  {keys} {}", row.description)));
            }
            rows.push((false, String::new()));
        }
        if marked {
            rows.push((false, "* set in config.toml".to_string()));
        }
        rows.push((
            false,
            "In the palette, the filter line and questions: Enter, Esc, ↑ and ↓.".to_string(),
        ));
        Self { rows, offset: 0 }
    }

    #[cfg(test)]
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub(super) fn key(&mut self, key: KeyEvent) -> ModalOutcome {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q' | '?') => ModalOutcome::Close,
            KeyCode::Down | KeyCode::Char('j') => {
                self.offset = (self.offset + 1).min(self.rows.len().saturating_sub(1));
                ModalOutcome::Stay
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.offset = self.offset.saturating_sub(1);
                ModalOutcome::Stay
            }
            _ => ModalOutcome::Stay,
        }
    }

    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let height = area.height.saturating_sub(2);
        let inner = boxed(frame, area, 80, height, "Keys", theme);
        let cols = usize::from(inner.width);
        let lines: Vec<Line> = self
            .rows
            .iter()
            .skip(self.offset)
            .map(|(heading, text)| {
                let style = if *heading { theme.accent } else { theme.text };
                Line::styled(fit(text, cols), style)
            })
            .collect();
        frame.render_widget(Paragraph::new(lines), inner);
    }
}
