//! `?`: the keys of the active view and the global ones, from the live keymap, with the keys
//! set in `config.toml` marked. It scrolls, and closes on the `global.help` key, `Esc` or `q`.

use crossterm::event::{KeyCode, KeyEvent};
use rail_core::config::Icons;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;

use super::scroll::ScrollBox;
use super::{ModalOutcome, boxed};
use crate::tui::keys::{Action, Binding, Key, Keymap, View};
use crate::tui::text::{escape, fit, pad};
use crate::tui::theme::Theme;

/// Columns of the keys column.
const KEY_COLS: usize = 20;
/// What `*` means, in the box title so it shows at every scroll position.
const LEGEND: &str = "* set in config.toml";

pub struct Help {
    /// (heading?, text) rows: headings are styled apart.
    rows: Vec<(bool, String)>,
    /// Some key is marked with `*`.
    marked: bool,
    /// The single keys bound to `global.help` (a chord cannot be pressed in here).
    close: Vec<Key>,
    scroll: ScrollBox,
}

impl Help {
    pub fn new(keymap: &Keymap, views: &[View], icons: Icons) -> Self {
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
        rows.push((
            false,
            "In the palette, the filter line and questions: Enter, Esc, ↑ and ↓.".to_string(),
        ));
        let close = keymap
            .bindings(Action::GlobalHelp)
            .iter()
            .filter_map(|binding| match binding {
                Binding::Single(key) => Some(*key),
                Binding::Chord(..) => None,
            })
            .collect();
        Self {
            rows,
            marked,
            close,
            scroll: ScrollBox::new(icons),
        }
    }

    /// The box title: with the legend when a key is marked.
    pub fn title(&self) -> String {
        if self.marked {
            format!("Keys ({LEGEND})")
        } else {
            "Keys".to_string()
        }
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
        let fixed = matches!(key.code, KeyCode::Esc | KeyCode::Char('q'));
        if fixed || self.close.contains(&Key::from_event(key)) {
            return ModalOutcome::Close;
        }
        self.scroll.key(key);
        ModalOutcome::Stay
    }

    pub(super) fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme) {
        let height = area.height.saturating_sub(2);
        let inner = boxed(frame, area, 80, height, &self.title(), theme);
        let cols = usize::from(inner.width);
        let lines = self
            .rows
            .iter()
            .map(|(heading, text)| {
                let style = if *heading { theme.accent } else { theme.text };
                Line::styled(fit(text, cols), style)
            })
            .collect();
        self.scroll.render(frame, inner, lines, theme);
    }
}
