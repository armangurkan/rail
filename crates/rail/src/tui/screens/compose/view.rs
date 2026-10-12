//! Drawing the review: the title, the rows with the cursor row highlighted, the key hint or
//! the discard question, and the open prompt over the bottom rows.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Span;
use ratatui::widgets::{Clear, Paragraph};

use super::review::{self, DISCARD_QUESTION, Row, RowStyle};
use super::{Composer, Prompting};
use crate::tui::screens::mail::Glyphs;
use crate::tui::text::{fit, pad};
use crate::tui::theme::Theme;

/// The first row shown so that `cursor` stays inside `visible` rows, moving `scroll` as
/// little as possible.
pub(super) fn top(cursor: usize, scroll: usize, visible: usize) -> usize {
    if cursor < scroll {
        cursor
    } else if visible > 0 && cursor >= scroll + visible {
        cursor + 1 - visible
    } else {
        scroll
    }
}

fn line(frame: &mut Frame, area: Rect, y: u16, span: Span<'_>) {
    frame.render_widget(Paragraph::new(span), Rect::new(area.x, y, area.width, 1));
}

impl Composer {
    /// The review over all of `area`: title, a blank row, the rows, the hint on the last row.
    pub fn render(&self, frame: &mut Frame, area: Rect, theme: &Theme, glyphs: &Glyphs) {
        if area.height < 3 || area.width == 0 {
            return;
        }
        frame.render_widget(Clear, area);
        let cols = usize::from(area.width);
        self.width.set(cols);
        let body = Rect::new(area.x, area.y + 2, area.width, area.height - 3);
        self.view_rows.set(usize::from(body.height));
        let title = review::title(&self.composing);
        line(
            frame,
            area,
            area.y,
            Span::styled(fit(&title, cols), theme.accent),
        );
        let rows = review::rows(&self.composing, cols, glyphs.attachment);
        self.render_rows(frame, body, theme, &rows);
        let last = area.y + area.height - 1;
        let hint = if self.confirm_discard {
            Span::styled(fit(DISCARD_QUESTION, cols), theme.error)
        } else {
            Span::styled(fit(&self.hint, cols), theme.muted)
        };
        line(frame, area, last, hint);
        match &self.prompting {
            Some(Prompting::Headers(state)) => state.prompt.render(frame, area, theme),
            Some(Prompting::Path(prompt)) => prompt.render(frame, area, theme),
            None => {}
        }
    }

    fn render_rows(&self, frame: &mut Frame, body: Rect, theme: &Theme, rows: &[Row]) {
        let visible = usize::from(body.height);
        let cursor = self.cursor.min(rows.len().saturating_sub(1));
        let first = top(cursor, self.scroll, visible);
        let cols = usize::from(body.width);
        for (offset, (index, row)) in rows
            .iter()
            .enumerate()
            .skip(first)
            .take(visible)
            .enumerate()
        {
            let style = match row.style {
                _ if index == cursor => theme.selected,
                RowStyle::Text => theme.text,
                RowStyle::Muted => theme.muted,
                RowStyle::Error => theme.error,
            };
            let y = body.y + u16::try_from(offset).unwrap_or(u16::MAX);
            line(frame, body, y, Span::styled(pad(&row.text, cols), style));
        }
    }
}
