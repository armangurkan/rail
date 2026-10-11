//! A scrollable, wrapped text panel with a scrollbar.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{
    Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, StatefulWidget, Widget,
};

use crate::tui::theme::Theme;

/// The scroll position of a text panel, in wrapped rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ScrollPanel {
    offset: u16,
}

/// A step a [`ScrollPanel`] takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    Up,
    Down,
    PageUp,
    PageDown,
    Top,
    Bottom,
}

impl ScrollPanel {
    /// Scroll one step. `lines` is the number of wrapped rows (see [`line_count`]) and `page`
    /// how far a page moves, also in rows. Returns true when the offset changed.
    pub fn scroll(&mut self, step: Scroll, lines: u16, page: u16) -> bool {
        let last = lines.saturating_sub(page);
        let next = match step {
            Scroll::Up => self.offset.saturating_sub(1),
            Scroll::Down => self.offset.saturating_add(1),
            Scroll::PageUp => self.offset.saturating_sub(page),
            Scroll::PageDown => self.offset.saturating_add(page),
            Scroll::Top => 0,
            Scroll::Bottom => last,
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

/// Rows `text` takes when word-wrapped to `width` columns, the unit `ScrollPanel` scrolls by.
pub fn line_count(text: &str, width: u16) -> u16 {
    u16::try_from(wrap_lines(text, width).len()).unwrap_or(u16::MAX)
}

/// Word-wrap `text` to `width` columns (at least one): break at spaces, split words longer
/// than the width, and keep blank lines.
fn wrap_lines(text: &str, width: u16) -> Vec<String> {
    let width = usize::from(width.max(1));
    let mut rows = Vec::new();
    for line in text.lines() {
        wrap_line(line, width, &mut rows);
    }
    rows
}

fn wrap_line(line: &str, width: usize, rows: &mut Vec<String>) {
    let mut current = String::new();
    let mut current_len = 0;
    for word in line.split(' ') {
        let chars: Vec<char> = word.chars().collect();
        if current_len > 0 && current_len + 1 + chars.len() <= width {
            current.push(' ');
            current.extend(&chars);
            current_len += 1 + chars.len();
            continue;
        }
        if current_len > 0 {
            rows.push(std::mem::take(&mut current));
        }
        let mut chunks = chars.chunks(width).peekable();
        current_len = 0;
        while let Some(chunk) = chunks.next() {
            if chunks.peek().is_some() {
                rows.push(chunk.iter().collect());
            } else {
                current = chunk.iter().collect();
                current_len = chunk.len();
            }
        }
    }
    rows.push(current);
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
        let rows = wrap_lines(self.text, text_area.width);
        let mut state = ScrollbarState::new(rows.len()).position(usize::from(self.offset));
        let lines: Vec<Line> = rows.into_iter().map(Line::from).collect();
        Paragraph::new(lines)
            .style(self.theme.text)
            .scroll((self.offset, 0))
            .render(text_area, buf);
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
    use crate::tui::test_util::{SIZES, render_lines};

    fn text() -> String {
        (1..=30).map(|n| format!("line {n}\n")).collect()
    }

    #[test]
    fn keys_scroll_and_clamp_to_the_text() {
        let mut panel = ScrollPanel::default();
        let lines = line_count(&text(), 40);
        assert_eq!(lines, 30);
        assert!(!panel.scroll(Scroll::Up, lines, 10), "already at the top");
        assert!(panel.scroll(Scroll::Down, lines, 10));
        assert!(panel.scroll(Scroll::PageDown, lines, 10));
        assert!(panel.scroll(Scroll::Bottom, lines, 10));
        assert!(!panel.scroll(Scroll::Down, lines, 10), "already at the end");
        assert!(panel.scroll(Scroll::Top, lines, 10));
    }

    #[test]
    fn renders_from_the_scrolled_offset_at_both_sizes() {
        let theme = Theme::default();
        let text = text();
        let mut panel = ScrollPanel::default();
        for _ in 0..3 {
            panel.scroll(Scroll::Down, line_count(&text, 40), 10);
        }
        for (width, height) in SIZES {
            let lines = render_lines(width, height, |frame| {
                frame.render_widget(panel.widget(&text, &theme), frame.area());
            });
            assert!(lines[0].starts_with("line 4 "), "{lines:?}");
            assert!(!lines.iter().any(|l| l.starts_with("line 3 ")), "{lines:?}");
        }
    }

    #[test]
    fn line_count_wraps_at_spaces_and_splits_long_words() {
        assert_eq!(line_count("aaaa bbbb cccc", 9), 2);
        assert_eq!(line_count("abcdefghij", 4), 3);
        assert_eq!(line_count("one\n\ntwo", 10), 3, "blank lines are kept");
        assert_eq!(line_count("", 10), 0);
    }

    #[test]
    fn degenerate_widths_do_not_panic_or_loop() {
        assert_eq!(line_count("abc def", 0), 6);
        assert_eq!(line_count("abc def", 1), 6);
    }

    #[test]
    fn end_reaches_the_tail_of_wrapped_text() {
        let theme = Theme::default();
        let text: String = (1..=5)
            .map(|n| {
                let words: Vec<String> = (0..30).map(|i| format!("w{n}x{i}")).collect();
                format!("{}\n", words.join(" "))
            })
            .collect();
        let inner = 79;
        let rows = line_count(&text, inner);
        assert!(rows > 10, "each source line should wrap to several rows");
        let mut panel = ScrollPanel::default();
        assert!(panel.scroll(Scroll::Bottom, rows, 10));
        let lines = render_lines(80, 10, |frame| {
            frame.render_widget(panel.widget(&text, &theme), frame.area());
        });
        assert!(lines[9].contains("w5x29"), "{lines:?}");
        assert!(
            lines[0].chars().take(79).any(|c| c != ' '),
            "a full page shows: {lines:?}"
        );
    }
}
