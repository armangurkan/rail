//! The two rows at the bottom of a screen: an optional spinner and message, then key hints.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::tui::theme::Theme;

/// Braille spinner frames, indexed by tick count modulo the length.
pub const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// A message row (with spinner while busy) above a muted row of key hints.
pub struct StatusLine<'a> {
    pub message: Option<&'a str>,
    /// The tick count while something is in flight; `None` hides the spinner.
    pub spinner: Option<usize>,
    pub hints: &'a str,
    pub theme: &'a Theme,
}

impl Widget for StatusLine<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let mut first = Vec::new();
        if let Some(tick) = self.spinner {
            let frame = SPINNER[tick % SPINNER.len()];
            first.push(Span::styled(format!("{frame} "), self.theme.accent));
        }
        if let Some(message) = self.message {
            first.push(Span::styled(message, self.theme.text));
        }
        let lines = vec![
            Line::from(first),
            Line::styled(self.hints, self.theme.muted),
        ];
        Paragraph::new(lines).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::{SIZES, render_lines};

    fn draw(spinner: Option<usize>, width: u16, height: u16) -> Vec<String> {
        let theme = Theme::default();
        render_lines(width, height, |frame| {
            let status = StatusLine {
                message: Some("Waiting for the browser login"),
                spinner,
                hints: "Esc back · Ctrl-C quit",
                theme: &theme,
            };
            frame.render_widget(status, frame.area());
        })
    }

    #[test]
    fn spinner_frame_follows_the_tick_at_both_sizes() {
        for (width, height) in SIZES {
            let lines = draw(Some(13), width, height);
            let expected = format!("{} Waiting for the browser login", SPINNER[3]);
            assert!(lines[0].starts_with(&expected), "{lines:?}");
            assert!(lines[1].starts_with("Esc back · Ctrl-C quit"), "{lines:?}");
        }
    }

    #[test]
    fn idle_status_has_no_spinner() {
        let lines = draw(None, 80, 24);
        assert!(
            lines[0].starts_with("Waiting for the browser login"),
            "{lines:?}"
        );
    }
}
