//! A vertical list of steps showing progress: done, current, upcoming or skipped.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};

use crate::tui::theme::Theme;

/// Where a step stands relative to the one the user is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepMark {
    Done,
    Current,
    Upcoming,
    Skipped,
}

impl StepMark {
    /// The one-character marker drawn in front of the step label.
    pub fn symbol(self) -> &'static str {
        match self {
            Self::Done => "✓",
            Self::Current => "▸",
            Self::Upcoming => " ",
            Self::Skipped => "–",
        }
    }
}

/// Renders one row per step, `marker label`.
pub struct StepList<'a> {
    steps: &'a [(&'a str, StepMark)],
    theme: &'a Theme,
}

impl<'a> StepList<'a> {
    pub fn new(steps: &'a [(&'a str, StepMark)], theme: &'a Theme) -> Self {
        Self { steps, theme }
    }
}

impl Widget for StepList<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let lines: Vec<Line> = self
            .steps
            .iter()
            .map(|(label, mark)| {
                let style = match mark {
                    StepMark::Current => self.theme.accent,
                    StepMark::Upcoming => self.theme.text,
                    StepMark::Done | StepMark::Skipped => self.theme.muted,
                };
                Line::styled(format!("{} {label}", mark.symbol()), style)
            })
            .collect();
        Paragraph::new(lines).render(area, buf);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::test_util::{SIZES, render_lines};

    const STEPS: [(&str, StepMark); 4] = [
        ("Start", StepMark::Done),
        ("Provider", StepMark::Skipped),
        ("Details", StepMark::Current),
        ("Login", StepMark::Upcoming),
    ];

    #[test]
    fn marks_every_step_at_both_sizes() {
        let theme = Theme::default();
        for (width, height) in SIZES {
            let lines = render_lines(width, height, |frame| {
                frame.render_widget(StepList::new(&STEPS, &theme), frame.area());
            });
            assert!(lines[0].starts_with("✓ Start"), "{lines:?}");
            assert!(lines[1].starts_with("– Provider"), "{lines:?}");
            assert!(lines[2].starts_with("▸ Details"), "{lines:?}");
            assert!(lines[3].starts_with("  Login"), "{lines:?}");
            assert!(lines[4].trim().is_empty(), "{lines:?}");
        }
    }

    #[test]
    fn current_step_uses_the_accent_style() {
        let theme = Theme::default();
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 4));
        StepList::new(&STEPS, &theme).render(buf.area, &mut buf);
        assert_eq!(buf[(2, 2)].style().fg, theme.accent.fg);
        assert_eq!(buf[(2, 0)].style().fg, theme.muted.fg);
    }
}
