//! Drawing the reader: the thread's subject, then the lines from the scroll offset, the cursor
//! line highlighted, and (where the terminal supports them) the link markers as OSC 8
//! hyperlinks.

use std::num::NonZeroU16;

use ratatui::Frame;
use ratatui::buffer::{Buffer, CellDiffOption};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use super::{LineKind, Reader, ReaderLine, Tone};
use crate::tui::screens::mail::visible_offset;
use crate::tui::text::{fit, markers, osc8, safe_link};
use crate::tui::theme::Theme;

fn style(tone: Tone, theme: &Theme) -> Style {
    match tone {
        Tone::Plain => theme.text,
        Tone::Strong => theme.text.add_modifier(Modifier::BOLD),
        Tone::Muted => theme.muted,
        Tone::Quote => theme.muted.add_modifier(Modifier::ITALIC),
        Tone::Error => theme.error,
    }
}

impl Reader {
    pub fn render(
        &self,
        frame: &mut Frame,
        area: Rect,
        theme: &Theme,
        hyperlinks: bool,
        focused: bool,
    ) {
        let [title, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(area);
        let count = self.detail.messages.len();
        let heading = format!(
            "{}  ({count} message{})",
            self.detail.subject,
            if count == 1 { "" } else { "s" }
        );
        frame.render_widget(
            Paragraph::new(Line::styled(
                fit(&heading, usize::from(title.width)),
                theme.accent,
            )),
            title,
        );
        self.size.set((body.width, body.height));
        let lines = self.lines(body.width);
        let height = usize::from(body.height);
        let scroll = visible_offset(self.scroll, self.cursor, height);
        let shown: Vec<Line> = lines
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
            .map(|(index, line)| {
                let base = style(line.tone, theme);
                let style = if focused && index == self.cursor {
                    theme.selected
                } else {
                    base
                };
                Line::styled(line.text.clone(), style)
            })
            .collect();
        frame.render_widget(Paragraph::new(shown), body);
        if hyperlinks {
            self.link_cells(frame.buffer_mut(), body, &lines[scroll.min(lines.len())..]);
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
