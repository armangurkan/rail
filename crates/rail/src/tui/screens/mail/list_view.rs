//! The thread list's rows and date groups. A row reads the dot, the account's badge (merged
//! mailboxes only), the people, `★` and `📎`, the subject with its count after it, and the
//! time right-aligned at the edge; a group row (`TODAY`, `YESTERDAY`, `THIS WEEK`, `OLDER`)
//! heads each run of threads in one group, and the group of what is on screen is always on the
//! first row. People, subjects and a failed action's text are outside text: `pad` and `fit`
//! escape them.

use rail_core::config::DimRead;
use rail_core::store::view::ThreadRow;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::view::{Glyphs, participants, people_cols, visible_offset};
use super::{AccountEntry, Focus, MailScreen};
use crate::tui::clock::Group;
use crate::tui::text::{fit, pad, pad_left, width};
use crate::tui::theme::Theme;

/// The dot and its space.
const DOT_COLS: usize = 2;
/// The two spaces after the people, and the two before the time.
const GAP_COLS: usize = 2;
/// `★` (or a space), `📎` (two cells, or two spaces) and a space.
const MARKS_COLS: usize = 4;

/// One drawn row of the thread list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ListLine {
    /// A group's name.
    Group(Group),
    /// The window's thread at this index.
    Thread(usize),
}

/// The window's threads (each one's group, in the window's order) with a group row wherever
/// the group changes from the thread before.
pub(super) fn list_lines(groups: &[Group]) -> Vec<ListLine> {
    let mut lines = Vec::with_capacity(groups.len() + 4);
    for (index, group) in groups.iter().enumerate() {
        if index == 0 || groups[index - 1] != *group {
            lines.push(ListLine::Group(*group));
        }
        lines.push(ListLine::Thread(index));
    }
    lines
}

/// The lines drawn in a pane `height` rows tall, the cursor on thread `selected` and the
/// remembered first thread `offset` (thread indices). The first row is the group row of the
/// line the pane starts at, drawn over that line when it is a thread. The pane starts one
/// line above the offset thread, moves just enough to show the cursor, never past the last
/// full screen, one line higher when the group row would cover the cursor, and one line lower
/// when the kept group row would head no thread on screen. One row shows the cursor's thread
/// alone. A group row that would end the pane is left off.
pub(super) fn frame_lines(
    lines: &[ListLine],
    offset: usize,
    selected: usize,
    height: usize,
) -> Vec<ListLine> {
    let line_of = |thread: usize| {
        lines
            .iter()
            .position(|l| *l == ListLine::Thread(thread))
            .unwrap_or(0)
    };
    let cursor = line_of(selected);
    if lines.is_empty() || height < 2 {
        return lines
            .get(cursor)
            .copied()
            .into_iter()
            .take(height)
            .collect();
    }
    let top = line_of(offset).saturating_sub(1);
    let last_screen = lines.len().saturating_sub(height);
    let mut start = visible_offset(top, cursor, height).min(last_screen);
    if start == cursor && matches!(lines[start], ListLine::Thread(_)) {
        start = start.saturating_sub(1);
    }
    // A start just above a group row would keep a group with nothing on screen over it. The
    // cursor is a thread below that row, so it stays in view.
    // At the end of the list this can leave the bottom row blank, accepted: every thread on
    // screen keeps its group row above it, which matters more than a full last screen.
    if matches!(lines.get(start + 1), Some(ListLine::Group(_))) {
        start += 1;
    }
    let group = lines[..=start].iter().rev().find_map(|line| match line {
        ListLine::Group(group) => Some(ListLine::Group(*group)),
        ListLine::Thread(_) => None,
    });
    let mut out: Vec<ListLine> = group
        .into_iter()
        .chain(lines[start + 1..].iter().copied().take(height - 1))
        .collect();
    // A group's name alone on the bottom row heads nothing on screen: the row stays blank and
    // the name heads the next page.
    if out.len() > 1 && matches!(out.last(), Some(ListLine::Group(_))) {
        out.pop();
    }
    out
}

/// The last thread a pane `height` rows tall draws when it starts at thread `top` with the
/// cursor on it: how far a page from `top` reaches.
pub(super) fn last_thread_from(lines: &[ListLine], top: usize, height: usize) -> usize {
    frame_lines(lines, top, top, height)
        .iter()
        .rev()
        .find_map(|line| match line {
            ListLine::Thread(index) => Some(*index),
            ListLine::Group(_) => None,
        })
        .unwrap_or(top)
}

impl MailScreen {
    /// The window's thread `index` as a row in a list `cols` wide, its time by `group`: the
    /// cursor, the multi-selection, a failed action, the badge in a merged mailbox.
    pub(super) fn thread_row(
        &self,
        index: usize,
        group: Group,
        cols: u16,
        theme: &Theme,
        glyphs: &Glyphs,
    ) -> Line<'static> {
        let Some(row) = self.list.rows().get(index) else {
            return Line::default();
        };
        let account = self.account_index_of(row.account_id);
        let badge = self.scope.is_unified().then(|| {
            account
                .and_then(|i| self.accounts.get(i))
                .map_or_else(|| " ".to_string(), AccountEntry::badge)
        });
        let ctx = RowContext {
            dim_read: self.ctx.dim_read,
            selected: index == self.list.selected_index() && self.focus != Focus::Sidebar,
            marked: self.actions.selection.contains(row.key.thread_id),
            failed: self
                .actions
                .failures
                .get(&row.key.thread_id)
                .map(|(_, text)| text.as_str()),
            badge,
            color: account.map_or(Color::Reset, |i| self.account_color(i, theme)),
            time: self.ctx.clock.time_in_group(row.key.last_date, group),
            time_cols: self.ctx.clock.time_cols(),
        };
        thread_line(row, &ctx, cols, theme, glyphs)
    }
}

/// What a thread row needs besides the thread.
pub(super) struct RowContext<'a> {
    /// The cursor row: `theme.selected` in every span (the caller leaves it off while the
    /// sidebar has focus).
    pub selected: bool,
    /// Which parts of a read row take the theme's muted colour.
    pub dim_read: DimRead,
    /// In the multi-selection: `▸` replaces the dot.
    pub marked: bool,
    /// A failed action's text, drawn in the subject's room.
    pub failed: Option<&'a str>,
    /// The account's badge letter, in a merged mailbox only.
    pub badge: Option<String>,
    /// The account's colour: the dot's, the mark's and the badge's.
    pub color: Color,
    /// The time for the thread's group (`Clock::time_in_group`).
    pub time: String,
    /// The time column's width (`Clock::time_cols`).
    pub time_cols: usize,
}

/// One thread's row in a list `cols` wide: the subject is cut to the room the other columns
/// leave (the count's width taken first, so the count is never cut), and the time ends at the
/// last column.
pub(super) fn thread_line(
    row: &ThreadRow,
    ctx: &RowContext<'_>,
    cols: u16,
    theme: &Theme,
    glyphs: &Glyphs,
) -> Line<'static> {
    let style = row_style(row, ctx, theme);
    let people_cols = people_cols(cols);
    let count = if row.message_count > 1 {
        format!(" ({})", row.message_count)
    } else {
        String::new()
    };
    let badge = ctx.badge.as_ref().map(|letter| format!(" {letter} "));
    let used = DOT_COLS
        + badge.as_ref().map_or(0, |b| width(b) + 1)
        + people_cols
        + GAP_COLS
        + MARKS_COLS
        + width(&count)
        + GAP_COLS
        + ctx.time_cols;
    let room = usize::from(cols).saturating_sub(used);
    let (text, text_style) = subject_text(row, ctx, style, theme);
    let subject = fit(text, room);
    let filler = " ".repeat(room.saturating_sub(width(&subject)) + GAP_COLS);
    // The cursor row is the selected style in every span: nothing low-contrast on it.
    let paint = |own: Style| if ctx.selected { style } else { own };
    let dot = dot_text(row, ctx, glyphs);
    let mut spans = vec![Span::styled(dot, paint(dot_style(row, ctx, style, theme)))];
    if let Some(badge) = badge {
        spans.push(Span::styled(
            badge,
            paint(Style::new().fg(Color::Black).bg(ctx.color)),
        ));
        spans.push(Span::styled(" ", style));
    }
    spans.push(Span::styled(
        people_and_marks(row, people_cols, glyphs),
        style,
    ));
    spans.push(Span::styled(subject, paint(text_style)));
    let time = pad_left(&ctx.time, ctx.time_cols);
    spans.push(Span::styled(format!("{count}{filler}{time}"), style));
    Line::from(spans)
}

/// An action error takes precedence over read-subject dimming.
fn subject_text<'a>(
    row: &'a ThreadRow,
    ctx: &RowContext<'a>,
    style: Style,
    theme: &Theme,
) -> (&'a str, Style) {
    match ctx.failed {
        Some(text) => (text, theme.error),
        None if row.unread_count == 0 && ctx.dim_read == DimRead::Subject => {
            (row.subject.as_str(), theme.muted)
        }
        None => (row.subject.as_str(), style),
    }
}

/// The row's style: selected on the cursor row, bold while the thread has unread mail.
fn row_style(row: &ThreadRow, ctx: &RowContext<'_>, theme: &Theme) -> Style {
    let base = if ctx.selected {
        theme.selected
    } else if row.unread_count == 0 && ctx.dim_read == DimRead::All {
        theme.muted
    } else {
        theme.text
    };
    if row.unread_count > 0 {
        base.add_modifier(Modifier::BOLD)
    } else {
        base
    }
}

/// `▸` on a thread in the multi-selection, `●` on an unread one, `·` on a read one; then a
/// space.
fn dot_text(row: &ThreadRow, ctx: &RowContext<'_>, glyphs: &Glyphs) -> String {
    let glyph = if ctx.marked {
        glyphs.marked
    } else if row.unread_count > 0 {
        glyphs.account
    } else {
        glyphs.read
    };
    format!("{glyph} ")
}

/// The dot's style off the cursor row: the account's colour on `▸` and `●`, the muted colour
/// on `·`.
fn dot_style(row: &ThreadRow, ctx: &RowContext<'_>, style: Style, theme: &Theme) -> Style {
    if ctx.marked || row.unread_count > 0 {
        style.fg(ctx.color)
    } else {
        style.fg(theme.muted.fg.unwrap_or(Color::Reset))
    }
}

/// The people padded to their column, two spaces, `★` and `📎` (or spaces), a space.
fn people_and_marks(row: &ThreadRow, people_cols: usize, glyphs: &Glyphs) -> String {
    let star = if row.starred { glyphs.star } else { " " };
    let clip = if row.has_attachments {
        glyphs.attachment
    } else {
        "  "
    };
    let people = pad(&participants(&row.participants), people_cols);
    format!("{people}  {star}{clip} ")
}

#[cfg(test)]
mod tests;
