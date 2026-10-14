//! Drawing the reader: its lines from the scroll offset (the title block first, so a long
//! newest message scrolls the subject away), each segment in its tone's style and the cursor
//! line highlighted, and (where the terminal supports them) the link markers as OSC 8
//! hyperlinks.

use std::num::NonZeroU16;

use ratatui::Frame;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use super::{LineKind, Reader, ReaderLine, Tone};
use crate::tui::screens::mail::visible_offset;
use crate::tui::text::{markers, osc8, safe_link};
use crate::tui::theme::Theme;

/// A segment's style in `tone`; `badge` is the account's colour.
fn style(tone: Tone, theme: &Theme, badge: Color) -> Style {
    match tone {
        Tone::Plain => theme.text,
        Tone::Strong => theme.text.add_modifier(Modifier::BOLD),
        Tone::Muted => theme.muted,
        Tone::Quote => theme.muted.add_modifier(Modifier::ITALIC),
        Tone::Error => theme.error,
        Tone::Accent => theme.accent.add_modifier(Modifier::BOLD),
        Tone::Badge => Style::new().fg(Color::Black).bg(badge),
        Tone::Number => number(theme),
    }
}

/// A card number's box: the theme's text colour on its border colour (a grey box in the
/// default theme).
fn number(theme: &Theme) -> Style {
    let style = theme
        .border
        .fg
        .map_or(Style::new(), |bg| Style::new().bg(bg));
    theme.text.fg.map_or(style, |fg| style.fg(fg))
}

/// `line` as drawn: each segment in its tone's style, or, on the cursor row, every segment and
/// the row itself in `theme.selected`.
fn styled(line: &ReaderLine, theme: &Theme, badge: Color, cursor: bool) -> Line<'static> {
    let spans: Vec<Span<'static>> = line
        .segments
        .iter()
        .map(|(text, tone)| {
            let style = if cursor {
                theme.selected
            } else {
                style(*tone, theme, badge)
            };
            Span::styled(text.clone(), style)
        })
        .collect();
    let row = if cursor { theme.selected } else { Style::new() };
    Line::from(spans).style(row)
}

impl Reader {
    /// The lines in `area` from the scroll offset, the cursor line highlighted while
    /// `focused`, and the link markers as OSC 8 hyperlinks when `hyperlinks`.
    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
        hyperlinks: bool,
        focused: bool,
    ) {
        self.size.set((area.width, area.height));
        let lines = self.lines(area.width);
        let height = usize::from(area.height);
        let scroll = visible_offset(self.scroll, self.cursor, height);
        let badge = self.account.color(theme);
        let shown: Vec<Line> = lines
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
            .map(|(index, line)| styled(line, theme, badge, focused && index == self.cursor))
            .collect();
        frame.render_widget(Paragraph::new(shown), area);
        if hyperlinks {
            self.link_cells(frame.buffer_mut(), area, &lines[scroll.min(lines.len())..]);
        }
    }

    /// Turn each marker of a visible body line into an OSC 8 hyperlink: the marker's first
    /// cell carries the whole sequence and claims the marker's width, so the diff writes it raw
    /// and skips the cells it covers. Only addresses `safe_link` accepts.
    fn link_cells(&self, buffer: &mut Buffer, area: Rect, visible: &[ReaderLine]) {
        for (row, line) in visible.iter().take(usize::from(area.height)).enumerate() {
            let message = match line.kind {
                LineKind::Body { message } | LineKind::Quote { message, .. } => message,
                _ => continue,
            };
            let Some(links) = self.detail.messages.get(message).map(|m| &m.links) else {
                continue;
            };
            for marker in markers(&line.text, links.len()) {
                let Some(url) = links.get(marker.number - 1).and_then(|a| safe_link(a)) else {
                    continue;
                };
                let (Ok(x), Ok(y), Ok(w)) = (
                    u16::try_from(marker.col),
                    u16::try_from(row),
                    u16::try_from(marker.width),
                ) else {
                    continue;
                };
                if x.saturating_add(w) > area.width {
                    continue;
                }
                let (Some(cell), Some(width)) = (
                    buffer.cell_mut((area.x + x, area.y + y)),
                    NonZeroU16::new(w),
                ) else {
                    continue;
                };
                cell.set_symbol(&osc8(&url, &format!("[{}]", marker.number)));
                cell.set_diff_option(CellDiffOption::ForcedWidth(width));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::screens::reader::ReaderAccount;

    /// A line of `segments`, its text their concatenation.
    fn line(segments: &[(&str, Tone)]) -> ReaderLine {
        let segments: Vec<(String, Tone)> = segments
            .iter()
            .map(|(text, tone)| ((*text).to_string(), *tone))
            .collect();
        ReaderLine {
            kind: LineKind::Note,
            text: segments.iter().map(|(t, _)| t.as_str()).collect(),
            segments,
        }
    }

    #[test]
    fn each_segment_is_drawn_in_its_tone_and_the_cursor_row_is_selected_throughout() {
        let theme = Theme::default();
        let tones = [
            Tone::Plain,
            Tone::Strong,
            Tone::Muted,
            Tone::Quote,
            Tone::Error,
            Tone::Accent,
            Tone::Badge,
            Tone::Number,
        ];
        let segments: Vec<(&str, Tone)> = tones.iter().map(|&tone| ("x", tone)).collect();
        let drawn = styled(&line(&segments), &theme, Color::Yellow, false);
        let styles: Vec<Style> = drawn.spans.iter().map(|s| s.style).collect();
        assert_eq!(
            styles,
            [
                theme.text,
                theme.text.add_modifier(Modifier::BOLD),
                theme.muted,
                theme.muted.add_modifier(Modifier::ITALIC),
                theme.error,
                theme.accent.add_modifier(Modifier::BOLD),
                Style::new().fg(Color::Black).bg(Color::Yellow),
                Style::new().bg(Color::DarkGray),
            ]
        );
        assert_eq!(drawn.style, Style::new(), "no row style off the cursor");
        let cursor = styled(&line(&segments), &theme, Color::Yellow, true);
        assert!(cursor.spans.iter().all(|s| s.style == theme.selected));
        assert_eq!(cursor.style, theme.selected);
        let text: String = cursor.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, "xxxxxxxx");
    }

    #[test]
    fn the_number_box_is_the_text_colour_on_the_border_colour() {
        let theme = Theme {
            text: Style::new().fg(Color::White),
            border: Style::new().fg(Color::Blue),
            ..Theme::default()
        };
        assert_eq!(
            number(&theme),
            Style::new().fg(Color::White).bg(Color::Blue)
        );
    }

    #[test]
    fn the_badge_takes_the_accounts_own_colour_else_the_themes_by_place() {
        let theme = Theme::default();
        let own = ReaderAccount {
            index: 1,
            color: Some(Color::Yellow),
            badge: "A".into(),
            label: "axon".into(),
        };
        assert_eq!(own.color(&theme), Color::Yellow);
        let by_place = ReaderAccount {
            color: None,
            ..own.clone()
        };
        assert_eq!(
            by_place.color(&theme),
            Color::Magenta,
            "the second account's"
        );
        let gone = ReaderAccount::unknown();
        assert_eq!(gone.color(&theme), Color::Reset);
        assert!(gone.badge.is_empty() && gone.label.is_empty());
    }
}
