//! A scrollable, wrapped text panel with a scrollbar.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{
    Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget, Wrap,
};

use crate::tui::theme::Theme;

/// The scroll position of a text panel, in source lines.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollPanel {
    offset: u16,
}

impl ScrollPanel {
    /// Scroll for a navigation key. `lines` is the length of the text and `page` how far a
    /// page key moves. Returns true when the offset changed.
    pub fn handle_key(&mut self, key: KeyEvent, lines: u16, page: u16) -> bool {
        let last = lines.saturating_sub(1);
        let next = match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.offset.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => self.offset.saturating_add(1),
            KeyCode::PageUp => self.offset.saturating_sub(page),
            KeyCode::PageDown => self.offset.saturating_add(page),
            KeyCode::Home => 0,
            KeyCode::End => last,
            _ => return false,
        }
        .min(last);
        let changed = next != self.offset;
        self.offset = next;
        changed
    }

    /// A renderable view of `text` at the current offset.
    pub fn widget<'a>(self, text: &'a str, theme: &'a Theme) -> ScrollView<'a> {
        ScrollView {
            text,
            offset: self.offset,
            theme,
        }
    }
}

/// Source lines in `text`, the unit `ScrollPanel` scrolls by.
pub fn line_count(text: &str) -> u16 {
    u16::try_from(text.lines().count()).unwrap_or(u16::MAX)
}

/// The renderable form of a [`ScrollPanel`].
pub struct ScrollView<'a> {
    text: &'a str,
    offset: u16,
    theme: &'a Theme,
}

impl Widget for ScrollView<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let [text_area, bar_area] =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(1)]).areas(area);
        Paragraph::new(self.text)
            .style(self.theme.text)
            .wrap(Wrap { trim: false })
            .scroll((self.offset, 0))
            .render(text_area, buf);
        let mut state = ScrollbarState::new(usize::from(line_count(self.text)))
            .position(usize::from(self.offset));
        StatefulWidget::render(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            bar_area,
            buf,
            &mut state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::{SIZES, key, render_lines};

    fn text() -> String {
        (1..=30).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn keys_scroll_and_clamp_to_the_text() {
        let mut panel = ScrollPanel::default();
        let lines = line_count(&text());
        assert_eq!(lines, 30);
        assert!(
            !panel.handle_key(key(KeyCode::Up), lines, 10),
            "already at the top"
        );
        assert!(panel.handle_key(key(KeyCode::Down), lines, 10));
        assert!(panel.handle_key(key(KeyCode::PageDown), lines, 10));
        assert!(panel.handle_key(key(KeyCode::End), lines, 10));
        assert!(
            !panel.handle_key(key(KeyCode::Down), lines, 10),
            "already at the end"
        );
        assert!(panel.handle_key(key(KeyCode::Home), lines, 10));
        assert!(!panel.handle_key(key(KeyCode::Char('x')), lines, 10));
    }

    #[test]
    fn renders_from_the_scrolled_offset_at_both_sizes() {
        let theme = Theme::default();
        let text = text();
        let mut panel = ScrollPanel::default();
        for _ in 0..3 {
            panel.handle_key(key(KeyCode::Down), line_count(&text), 10);
        }
        for (width, height) in SIZES {
            let lines = render_lines(width, height, |frame| {
                frame.render_widget(panel.widget(&text, &theme), frame.area());
            });
            assert!(lines[0].starts_with("line 4 "), "{lines:?}");
            assert!(!lines.iter().any(|l| l.starts_with("line 3 ")), "{lines:?}");
        }
    }
}
