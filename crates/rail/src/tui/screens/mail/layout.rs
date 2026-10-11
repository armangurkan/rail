//! Where things go: a header row, the sidebar (when shown), the list, the reader pane (when
//! split), and the status row.

use ratatui::layout::{Constraint, Layout, Rect};

/// Under this many columns the sidebar starts hidden.
pub const SIDEBAR_MIN_COLUMNS: u16 = 100;
/// The sidebar's width, its border included.
pub const SIDEBAR_WIDTH: u16 = 26;
/// The list's share of the width beside the reader pane, in percent.
const LIST_SHARE: u16 = 45;

pub struct Areas {
    pub header: Rect,
    pub sidebar: Option<Rect>,
    pub list: Rect,
    pub reader: Option<Rect>,
    pub status: Rect,
}

/// The reader goes beside the list at `threshold` columns and wider.
pub fn is_split(width: u16, threshold: u16) -> bool {
    width >= threshold
}

pub fn sidebar_by_default(width: u16) -> bool {
    width >= SIDEBAR_MIN_COLUMNS
}

pub fn areas(area: Rect, sidebar: bool, pane: bool) -> Areas {
    let [header, body, status] = Layout::vertical([
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
        sidebar: side,
        list,
        reader,
        status,
    }
}
