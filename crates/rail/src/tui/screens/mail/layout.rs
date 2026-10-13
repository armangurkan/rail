//! Where things go: a header row, the body (the sidebar when shown, the list, the reader pane
//! when split) and the hint row. In split mode every pane is boxed; the focused one's border
//! takes the theme's focus style.

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::widgets::{Block, Borders};

use crate::tui::theme::Theme;

/// Under this many columns the sidebar starts hidden.
pub const SIDEBAR_MIN_COLUMNS: u16 = 100;
/// The sidebar's width, its border included.
pub const SIDEBAR_WIDTH: u16 = 26;
/// The list's share of the width beside the reader pane, in percent.
const LIST_SHARE: u16 = 45;

pub struct Areas {
    pub header: Rect,
    /// Everything between the header and the hint row.
    pub body: Rect,
    pub sidebar: Option<Rect>,
    pub list: Rect,
    pub reader: Option<Rect>,
    pub hints: Rect,
}

/// The reader goes beside the list at `threshold` columns and wider.
pub fn is_split(width: u16, threshold: u16) -> bool {
    width >= threshold
}

/// Panes are boxed in split mode only.
pub fn bordered(width: u16, threshold: u16) -> bool {
    is_split(width, threshold)
}

pub fn sidebar_by_default(width: u16) -> bool {
    width >= SIDEBAR_MIN_COLUMNS
}

/// A border needs a cell on each side and one row of content inside.
pub fn fits_border(area: Rect) -> bool {
    area.width >= 4 && area.height >= 3
}

/// A pane's box: the focused pane's in `border_focus`, the others' in `border`.
pub fn pane_block(theme: &Theme, focused: bool) -> Block<'static> {
    let style = if focused {
        theme.border_focus
    } else {
        theme.border
    };
    Block::new().borders(Borders::ALL).border_style(style)
}

pub fn areas(area: Rect, sidebar: bool, pane: bool) -> Areas {
    let [header, body, hints] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(1),
    ])
    .areas(area);
    let (side, rest) = if sidebar {
        let [side, rest] =
            Layout::horizontal([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(0)]).areas(body);
        (Some(side), rest)
    } else {
        (None, body)
    };
    let (list, reader) = if pane {
        let [list, reader] = Layout::horizontal([
            Constraint::Percentage(LIST_SHARE),
            Constraint::Percentage(100 - LIST_SHARE),
        ])
        .areas(rest);
        (list, Some(reader))
    } else {
        (rest, None)
    };
    Areas {
        header,
        body,
        sidebar: side,
        list,
        reader,
        hints,
    }
}
