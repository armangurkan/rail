//! Scrolling for the read-only boxes (help and `:errors`): the keys they share, the offset,
//! and the `↓ more` / `↑ more` rows that say rows are hidden below or above.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent};
use rail_core::config::Icons;
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::widgets::Paragraph;

use crate::tui::theme::Theme;

/// What a box shows of its rows at one offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Window {
    /// The first row shown.
    pub(super) first: usize,
    /// Rows shown between the hints.
    pub(super) rows: usize,
    pub(super) above: bool,
    pub(super) below: bool,
}

/// The part of `total` rows a box of `height` rows shows from `offset`. A hint takes the top
/// row while rows are hidden above and the bottom row while rows are hidden below; the offset
/// is clamped so the last position ends at the last row. Under three rows there is no room
/// for hints. Once the top hint appears (offset 1 and later) the first row shown is `offset + 1`,
/// so each step down brings one new row at the bottom despite the row the hint took.
pub(super) fn window(total: usize, offset: usize, height: usize) -> Window {
    if total <= height || height < 3 {
        let first = offset.min(total.saturating_sub(height));
        let rows = height.min(total - first);
        return Window {
            first,
            rows,
            above: false,
            below: false,
        };
    }
    let last_first = total - (height - 1);
    let first = if offset == 0 {
        0
    } else {
        (offset + 1).min(last_first)
    };
    let above = first > 0;
    let room = height - usize::from(above);
    let below = first + room < total;
    Window {
        first,
        rows: room - usize::from(below),
        above,
        below,
    }
}

/// The scroll position of one box. Drawing clamps it, and drawing only borrows the box, so
/// the offset is a `Cell`.
pub(super) struct ScrollBox {
    offset: Cell<usize>,
    icons: Icons,
}

impl ScrollBox {
    pub(super) fn new(icons: Icons) -> Self {
        Self {
            offset: Cell::new(0),
            icons,
        }
    }

    /// `j`/`↓` one row down, `k`/`↑` one row up. True when the key was one of them.
    pub(super) fn key(&self, key: KeyEvent) -> bool {
        let offset = self.offset.get();
        let next = match key.code {
            KeyCode::Down | KeyCode::Char('j') => offset.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => offset.saturating_sub(1),
            _ => return false,
        };
        self.offset.set(next);
        true
    }

    /// Draw the visible part of `rows` in `area`, with the hints.
    pub(super) fn render(&self, frame: &mut Frame, area: Rect, rows: Vec<Line<'_>>, theme: &Theme) {
        let shown = window(rows.len(), self.offset.get(), usize::from(area.height));
        // With the top hint `first` is one past the offset (see `window`).
        self.offset
            .set(shown.first.saturating_sub(usize::from(shown.above)));
        let (up, down) = match self.icons {
            Icons::Unicode => ("↑ more", "↓ more"),
            Icons::Ascii => ("^ more", "v more"),
        };
        let mut lines = Vec::with_capacity(usize::from(area.height));
        if shown.above {
            lines.push(Line::styled(up, theme.muted));
        }
        lines.extend(rows.into_iter().skip(shown.first).take(shown.rows));
        if shown.below {
            lines.push(Line::styled(down, theme.muted));
        }
        frame.render_widget(Paragraph::new(lines), area);
    }
}
