//! Where things go: a header row (a box of three rows in split mode), the body (the sidebar
//! when shown, the list, the reader pane when split) and the hint bar. In split mode every pane
//! is boxed, the focused one's border in the theme's focus style, and the hint bar takes three
//! rows so each key sits in its own box, unless the hints are off.

use rail_core::config::Icons;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::symbols::border;
use ratatui::widgets::{Block, Borders};

use crate::tui::theme::Theme;

/// Under this many columns the sidebar starts hidden.
pub const SIDEBAR_MIN_COLUMNS: u16 = 100;
/// The sidebar's width, its border included.
pub const SIDEBAR_WIDTH: u16 = 26;
/// The list's share of the width beside the reader pane, in percent.
const LIST_SHARE: u16 = 45;
/// The hint bar's rows when each key sits in its own box (split mode).
pub const HINT_ROWS_BOXED: u16 = 3;
/// The header's rows when it is boxed (split mode).
pub const HEADER_ROWS_BOXED: u16 = 3;
/// Under this many rows the split-mode hint bar stays on one row, without key boxes.
pub const MIN_ROWS_FOR_KEY_BOXES: u16 = 12;

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

/// The box `icons = "ascii"` draws: `+` corners, `-` and `|` sides.
const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// The lines boxes and rules are drawn with.
pub fn border_set(icons: Icons) -> border::Set<'static> {
    match icons {
        Icons::Unicode => border::PLAIN,
        Icons::Ascii => ASCII_BORDER,
    }
}

/// A pane's box: the focused pane's in `border_focus`, the others' in `border`; ASCII lines
/// with `icons = "ascii"`.
pub fn pane_block(theme: &Theme, icons: Icons, focused: bool) -> Block<'static> {
    let style = if focused {
        theme.border_focus
    } else {
        theme.border
    };
    Block::new()
        .borders(Borders::ALL)
        .border_set(border_set(icons))
        .border_style(style)
}

/// The hint bar's rows: three (a box around each key) in split mode with the hints on when the
/// screen has `MIN_ROWS_FOR_KEY_BOXES` rows, else one.
fn hint_rows(area: Rect, pane: bool, hints: bool) -> u16 {
    if pane && hints && area.height >= MIN_ROWS_FOR_KEY_BOXES {
        HINT_ROWS_BOXED
    } else {
        1
    }
}

/// The header's rows: three (a box around the row) in split mode when the screen has
/// `MIN_ROWS_FOR_KEY_BOXES` rows, else one.
fn header_rows(area: Rect, pane: bool) -> u16 {
    if pane && area.height >= MIN_ROWS_FOR_KEY_BOXES {
        HEADER_ROWS_BOXED
    } else {
        1
    }
}

/// The frame's areas; `hints` is whether the key hints are shown (`hints = false` keeps the bar
/// to one row).
pub fn areas(area: Rect, sidebar: bool, pane: bool, hints: bool) -> Areas {
    let [header, body, hints] = Layout::vertical([
        Constraint::Length(header_rows(area, pane)),
        Constraint::Min(0),
        Constraint::Length(hint_rows(area, pane, hints)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_header_is_boxed_in_three_rows_only_when_split_and_tall_enough() {
        let header = |w, h, pane| areas(Rect::new(0, 0, w, h), false, pane, true).header;
        assert_eq!(header(150, 40, true).height, HEADER_ROWS_BOXED);
        assert_eq!(header(150, 11, true).height, 1, "under 12 rows");
        assert_eq!(header(90, 24, false).height, 1, "stacked");
    }
}
