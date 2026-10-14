//! The thread list's rows and date groups. A row reads the dot, the account's badge (merged
//! mailboxes only), the people, `★` and `📎`, the subject with its count after it, and the
//! time right-aligned at the edge; a group row (`TODAY`, `YESTERDAY`, `THIS WEEK`, `OLDER`)
//! heads each run of threads in one group, and the group of what is on screen is always on the
//! first row. People, subjects and a failed action's text are outside text: `pad` and `fit`
//! escape them.

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
    let style = row_style(row, ctx.selected, theme);
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
    let (text, text_style) = match ctx.failed {
        Some(text) => (text, theme.error),
        None => (row.subject.as_str(), style),
    };
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

/// The row's style: selected on the cursor row, bold while the thread has unread mail.
fn row_style(row: &ThreadRow, selected: bool, theme: &Theme) -> Style {
    let base = if selected { theme.selected } else { theme.text };
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
mod tests {
    use rail_core::config::Icons;
    use rail_core::provider::Mailbox;
    use rail_core::store::view::ThreadKey;
    use ratatui::buffer::Buffer;

    use super::ListLine::{Group as G, Thread as T};
    use super::*;
    use crate::tui::clock::Group::{Older, ThisWeek, Today, Yesterday};

    const DANA: &str = "Dana Reyes";

    /// A starred thread with an attachment, from Dana Reyes.
    fn thread(subject: &str, messages: u32, unread: u32) -> ThreadRow {
        ThreadRow {
            key: ThreadKey {
                last_date: 1_760_100_000,
                thread_id: 7,
            },
            account_id: 1,
            subject: subject.into(),
            participants: vec![Mailbox {
                name: Some(DANA.into()),
                address: "dana@example.com".into(),
            }],
            message_count: messages,
            unread_count: unread,
            starred: true,
            has_attachments: true,
        }
    }

    /// Not the cursor, not selected, no failure, cyan.
    fn context(badge: Option<&str>, time: &str) -> RowContext<'static> {
        RowContext {
            selected: false,
            marked: false,
            failed: None,
            badge: badge.map(str::to_string),
            color: Color::Cyan,
            time: time.into(),
            time_cols: 10,
        }
    }

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// `line` drawn on a terminal one row tall and `cols` wide.
    fn drawn(line: Line<'static>, cols: u16) -> Buffer {
        let backend = ratatui::backend::TestBackend::new(cols, 1);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| {
                frame.render_widget(ratatui::widgets::Paragraph::new(line), frame.area());
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    #[test]
    fn a_unified_row_reads_dot_badge_people_marks_subject_count_and_time() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let row = thread("Q4 roadmap review", 3, 1);
        for (cols, people, filler) in [(150u16, 24, 83), (90, 22, 25)] {
            let line = thread_line(&row, &context(Some("W"), "11:54"), cols, &theme, &glyphs);
            let expected = format!(
                "●  W  {DANA:<people$}  ★📎 Q4 roadmap review (3){}     11:54",
                " ".repeat(filler)
            );
            assert_eq!(text(&line), expected, "at {cols}");
            assert_eq!(width(&text(&line)), usize::from(cols));
            let cells = drawn(line, cols);
            assert_eq!(cells[(0, 0)].symbol(), "●");
            assert_eq!(cells[(0, 0)].fg, Color::Cyan, "the account's colour");
            assert!(cells[(0, 0)].modifier.contains(Modifier::BOLD), "unread");
            for x in 2..=4 {
                assert_eq!(cells[(x, 0)].bg, Color::Cyan, "the badge at {x}");
            }
            assert_eq!(cells[(3, 0)].symbol(), "W");
            assert_eq!(cells[(3, 0)].fg, Color::Black);
            assert_eq!(
                cells[(5, 0)].bg,
                Color::Reset,
                "a plain space after the badge"
            );
            assert_eq!(cells[(6, 0)].symbol(), "D", "the people from column 6");
            assert!(cells[(6, 0)].modifier.contains(Modifier::BOLD));
        }
    }

    /// `time = "12h"`: the column is two cells wider, the subject two cells narrower.
    #[test]
    fn a_twelve_hour_time_widens_the_column_and_the_subject_gives_the_cells() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let row = thread("Q4 roadmap review", 3, 1);
        let ctx = RowContext {
            time_cols: 12,
            ..context(Some("W"), "Wed 12:53 pm")
        };
        let line = thread_line(&row, &ctx, 90, &theme, &glyphs);
        let expected = format!(
            "●  W  {DANA:<22}  ★📎 Q4 roadmap review (3){}  Wed 12:53 pm",
            " ".repeat(21)
        );
        assert_eq!(text(&line), expected);
        assert_eq!(width(&text(&line)), 90);
        let short = RowContext {
            time_cols: 12,
            ..context(Some("W"), "9:53 am")
        };
        let line = thread_line(&row, &short, 90, &theme, &glyphs);
        assert!(text(&line).ends_with("       9:53 am"), "{}", text(&line));
        assert_eq!(width(&text(&line)), 90);
    }

    #[test]
    fn one_accounts_row_has_no_badge_and_gives_the_subject_its_cells() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let row = thread(&"x".repeat(100), 3, 1);
        let unified = text(&thread_line(
            &row,
            &context(Some("W"), "11:54"),
            90,
            &theme,
            &glyphs,
        ));
        let own = text(&thread_line(
            &row,
            &context(None, "11:54"),
            90,
            &theme,
            &glyphs,
        ));
        assert!(own.starts_with(&format!("● {DANA:<22}  ★📎 x")), "{own}");
        assert!(
            own.contains(&format!(" {}… (3)  ", "x".repeat(43))),
            "{own}"
        );
        assert!(
            unified.contains(&format!(" {}… (3)  ", "x".repeat(39))),
            "{unified}"
        );
        assert_eq!(width(&own), 90);
        assert_eq!(width(&unified), 90);
    }

    #[test]
    fn the_dot_takes_the_account_colour_unread_the_muted_colour_read_and_the_cursor_row_is_selected_throughout()
     {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let read = thread("Re: offsite", 1, 0);
        let line = thread_line(&read, &context(Some("W"), "Wed 12:53"), 90, &theme, &glyphs);
        let cells = drawn(line, 90);
        assert_eq!(cells[(0, 0)].symbol(), "·");
        assert_eq!(cells[(0, 0)].fg, theme.muted.fg.unwrap());
        assert!(
            !cells[(6, 0)].modifier.contains(Modifier::BOLD),
            "a read row is not bold"
        );
        assert_eq!(
            cells[(3, 0)].bg,
            Color::Cyan,
            "the badge keeps the account's colour"
        );
        let marked = RowContext {
            marked: true,
            ..context(Some("W"), "Wed 12:53")
        };
        let cells = drawn(thread_line(&read, &marked, 90, &theme, &glyphs), 90);
        assert_eq!(cells[(0, 0)].symbol(), "▸");
        assert_eq!(cells[(0, 0)].fg, Color::Cyan);
        // The cursor row is the selected style in every span, the read dot and the badge too.
        let cursor = RowContext {
            selected: true,
            color: Color::Magenta,
            failed: Some("archive failed"),
            ..context(Some("W"), "11:54")
        };
        let cells = drawn(thread_line(&read, &cursor, 90, &theme, &glyphs), 90);
        assert_eq!(cells[(0, 0)].symbol(), "·");
        let (fg, bg) = (theme.selected.fg.unwrap(), theme.selected.bg.unwrap());
        for x in [0u16, 3, 5, 6, 35, 89] {
            assert_eq!(
                (cells[(x, 0)].fg, cells[(x, 0)].bg),
                (fg, bg),
                "the cursor row at {x}"
            );
        }
        let ascii = Glyphs::of(Icons::Ascii);
        let line = thread_line(&read, &context(None, "Wed 12:53"), 90, &theme, &ascii);
        assert!(text(&line).starts_with(". Dana Reyes"), "{}", text(&line));
    }

    /// Review Focus 2.
    #[test]
    fn a_long_subject_is_cut_before_its_count_which_stays_whole() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let row = thread(&"x".repeat(100), 12, 1);
        let shown = text(&thread_line(
            &row,
            &context(Some("W"), "11:54"),
            90,
            &theme,
            &glyphs,
        ));
        assert_eq!(width(&shown), 90, "{shown}");
        assert!(
            shown.ends_with(&format!(" {}… (12)       11:54", "x".repeat(38))),
            "{shown}"
        );
        let one = thread(&"x".repeat(100), 1, 1);
        let shown = text(&thread_line(
            &one,
            &context(Some("W"), "11:54"),
            90,
            &theme,
            &glyphs,
        ));
        assert!(
            shown.ends_with(&format!(" {}…       11:54", "x".repeat(43))),
            "no count: the subject takes its cells: {shown}"
        );
    }

    #[test]
    fn a_failed_actions_text_takes_the_subject_room_in_the_error_style_and_keeps_the_count() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let row = thread("Q4 roadmap review", 3, 1);
        let failed = RowContext {
            failed: Some("archive failed: offline"),
            ..context(Some("W"), "11:54")
        };
        let line = thread_line(&row, &failed, 90, &theme, &glyphs);
        let shown = text(&line);
        assert!(
            shown.contains("★📎 archive failed: offline (3)  "),
            "{shown}"
        );
        assert!(!shown.contains("Q4 roadmap"), "{shown}");
        assert_eq!(width(&shown), 90);
        let error = line
            .spans
            .iter()
            .find(|s| s.content == "archive failed: offline")
            .expect("the failure's span");
        assert_eq!(error.style, theme.error);
        let count = line
            .spans
            .iter()
            .find(|s| s.content.starts_with(" (3)"))
            .expect("the count's span");
        assert_ne!(
            count.style.fg, theme.error.fg,
            "the count is not part of the failure"
        );
    }

    /// Review Focus 2 (the time).
    #[test]
    fn the_time_is_right_aligned_at_the_edge_at_90_and_150_columns() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let row = thread(&"x".repeat(200), 3, 1);
        for cols in [90u16, 150] {
            for time in ["11:54", "Wed 12:53", "Sep 09", "2024-09-04"] {
                let line = thread_line(&row, &context(Some("W"), time), cols, &theme, &glyphs);
                let shown = text(&line);
                assert_eq!(width(&shown), usize::from(cols), "{shown}");
                assert!(shown.ends_with(&format!("(3)  {time:>10}")), "{shown}");
                let cells = drawn(line, cols);
                let edge: String = (cols - 10..cols).map(|x| cells[(x, 0)].symbol()).collect();
                assert_eq!(edge, format!("{time:>10}"));
            }
        }
    }

    /// Review Focus 1.
    #[test]
    fn outside_text_in_a_thread_row_is_escaped() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let mut row = thread("pay\u{202e}fdp.exe \u{1b}[2J urgent", 1, 1);
        row.participants = vec![Mailbox {
            name: Some("Evil\u{7}Corp".into()),
            address: "e@example.com".into(),
        }];
        let shown = text(&thread_line(
            &row,
            &context(Some("W"), "Sep 09"),
            150,
            &theme,
            &glyphs,
        ));
        assert!(shown.contains("Evil\\u{7}Corp"), "{shown}");
        assert!(
            shown.contains("pay\\u{202e}fdp.exe \\u{1b}[2J urgent"),
            "{shown}"
        );
        let failed = RowContext {
            failed: Some("bad\u{1b}[31m"),
            ..context(Some("W"), "Sep 09")
        };
        let shown = text(&thread_line(&row, &failed, 150, &theme, &glyphs));
        assert!(shown.contains("bad\\u{1b}[31m"), "{shown}");
        assert!(
            !shown
                .chars()
                .any(|c| c == '\u{1b}' || c == '\u{7}' || c == '\u{202e}'),
            "{shown}"
        );
    }

    /// Review Focus 2 (narrow).
    #[test]
    fn a_row_narrower_than_its_columns_does_not_panic() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let row = thread("Q4", 12, 1);
        for cols in [0u16, 3, 30] {
            let line = thread_line(&row, &context(Some("W"), "11:54"), cols, &theme, &glyphs);
            assert!(text(&line).contains(" (12)"), "the count is still drawn");
            let _ = drawn(line, cols.max(1));
        }
    }

    /// The columns at which a unified row with a " (12)" count leaves its subject no room:
    /// dot 2 + badge 4 + people 12 + gap 2 + marks 4 + count 5 + gap 2 + time 10.
    const NO_ROOM_COLS: u16 = 41;

    #[test]
    fn at_the_width_where_the_subjects_room_is_zero_the_row_is_the_count_alone() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let row = thread("Q4 roadmap", 12, 1);
        let line = thread_line(
            &row,
            &context(Some("W"), "11:54"),
            NO_ROOM_COLS,
            &theme,
            &glyphs,
        );
        let shown = text(&line);
        assert_eq!(width(&shown), usize::from(NO_ROOM_COLS), "{shown}");
        assert!(!shown.contains("Q4") && !shown.contains('…'), "{shown}");
        assert!(shown.contains(" (12)"), "the count whole: {shown}");
        let _ = drawn(line, NO_ROOM_COLS);
    }

    #[test]
    fn a_subject_exactly_as_wide_as_its_room_is_drawn_in_full() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        // At 90 columns the room is 44 cells (the long-subject test: 43 and the cut mark).
        let exact = thread(&"x".repeat(44), 1, 1);
        let shown = text(&thread_line(
            &exact,
            &context(Some("W"), "11:54"),
            90,
            &theme,
            &glyphs,
        ));
        assert!(
            shown.contains(&"x".repeat(44)) && !shown.contains('…'),
            "{shown}"
        );
        let over = thread(&"x".repeat(45), 1, 1);
        let shown = text(&thread_line(
            &over,
            &context(Some("W"), "11:54"),
            90,
            &theme,
            &glyphs,
        ));
        assert!(shown.contains('…'), "one cell over is cut: {shown}");
    }

    #[test]
    fn a_wide_grapheme_subject_is_cut_by_cell_width_not_characters() {
        let (theme, glyphs) = (Theme::default(), Glyphs::of(Icons::Unicode));
        let row = thread(&"漢".repeat(40), 1, 1);
        let shown = text(&thread_line(
            &row,
            &context(Some("W"), "11:54"),
            90,
            &theme,
            &glyphs,
        ));
        assert_eq!(width(&shown), 90, "{shown}");
        assert!(shown.contains('…'), "{shown}");
        assert!(
            shown.contains(&"漢".repeat(21)),
            "43 cells hold 21: {shown}"
        );
        assert!(!shown.contains(&"漢".repeat(22)), "{shown}");
    }

    #[test]
    fn a_group_row_heads_each_run_of_threads_in_one_group() {
        let lines = list_lines(&[Today, Today, Yesterday, ThisWeek, Older, Older]);
        assert_eq!(
            lines,
            [
                G(Today),
                T(0),
                T(1),
                G(Yesterday),
                T(2),
                G(ThisWeek),
                T(3),
                G(Older),
                T(4),
                T(5)
            ]
        );
        let clock =
            crate::tui::clock::Clock::fixed(crate::tui::test_util::NOW, jiff::tz::TimeZone::UTC);
        let newest_first = [86_400, 0, -86_400, -3 * 86_400, -30 * 86_400]
            .map(|ago: i64| clock.group(crate::tui::test_util::NOW + ago));
        let groups: Vec<ListLine> = list_lines(&newest_first)
            .into_iter()
            .filter(|l| matches!(l, ListLine::Group(_)))
            .collect();
        assert_eq!(
            groups,
            [G(Today), G(Yesterday), G(ThisWeek), G(Older)],
            "a thread dated after today joins TODAY: no group repeats"
        );
        assert!(list_lines(&[]).is_empty());
    }

    /// Spec §5 (C3): a date the clock cannot place groups by its sign; OLDER heads the list
    /// once.
    #[test]
    fn dates_beyond_the_clocks_range_group_by_their_sign() {
        let now = crate::tui::test_util::NOW;
        let clock = crate::tui::clock::Clock::fixed(now, jiff::tz::TimeZone::UTC);
        let newest_first = [i64::MAX, now, now - 30 * 86_400, i64::MIN].map(|s| clock.group(s));
        let groups: Vec<ListLine> = list_lines(&newest_first)
            .into_iter()
            .filter(|l| matches!(l, ListLine::Group(_)))
            .collect();
        assert_eq!(groups, [G(Today), G(Older)], "OLDER heads the list once");
    }

    /// Spec §5 (C2): a group row never sits alone on the bottom row; it heads the next page.
    #[test]
    fn a_group_row_is_never_alone_on_the_bottom_row() {
        // [G(Today), T0, T1, T2, G(Yesterday), T3, T4]: five rows from the top would end on
        // YESTERDAY's row.
        let lines = list_lines(&[Today, Today, Today, Yesterday, Yesterday]);
        assert_eq!(
            frame_lines(&lines, 0, 0, 5),
            [G(Today), T(0), T(1), T(2)],
            "the row is left blank"
        );
        assert_eq!(
            frame_lines(&lines, 0, 0, 6),
            [G(Today), T(0), T(1), T(2), G(Yesterday), T(3)],
            "with a thread under it the heading stays"
        );
    }

    /// Review Focus 4.
    #[test]
    fn frame_lines_keep_the_cursor_in_view_with_group_rows_counted() {
        // [G(Today), T0, T1, T2, G(Yesterday), T3, T4, T5]
        let lines = list_lines(&[Today, Today, Today, Yesterday, Yesterday, Yesterday]);
        assert_eq!(frame_lines(&lines, 0, 0, 8), lines, "everything fits");
        assert_eq!(
            frame_lines(&lines, 0, 3, 4),
            [G(Today), T(2), G(Yesterday), T(3)],
            "down onto YESTERDAY: group rows at the top and in the middle, the cursor last"
        );
        assert_eq!(
            frame_lines(&lines, 4, 3, 4),
            [G(Yesterday), T(3), T(4), T(5)],
            "never past the last full screen"
        );
        assert_eq!(
            frame_lines(&lines, 2, 1, 3),
            [G(Today), T(1), T(2)],
            "the group row on top never covers the cursor"
        );
        assert_eq!(
            frame_lines(&lines, 4, 4, 3),
            [G(Yesterday), T(4), T(5)],
            "scrolled inside a group, its row stays on top"
        );
        assert_eq!(
            frame_lines(&lines, 0, 4, 1),
            [T(4)],
            "one row: the cursor's thread"
        );
        assert!(frame_lines(&lines, 0, 0, 0).is_empty());
        assert!(frame_lines(&[], 0, 0, 5).is_empty());
        // [G(Today), T0..T9, G(Yesterday), T10..T19]: the third page down from the top, five
        // rows, would start on T9, the last of TODAY, right above YESTERDAY's row.
        let ten_and_ten = list_lines(&[[Today; 10], [Yesterday; 10]].concat());
        assert_eq!(
            frame_lines(&ten_and_ten, 9, 12, 5),
            [G(Yesterday), T(10), T(11), T(12), T(13)],
            "a kept group row heads a thread on screen: no TODAY over YESTERDAY"
        );
        // [G(Today), T0, T1, T2, G(Yesterday), T3..T7]: the last full screen would start on T2,
        // right above YESTERDAY's row; it starts on that row instead and leaves one row blank.
        let three_and_five = list_lines(&[[Today; 3].as_slice(), &[Yesterday; 5]].concat());
        let last = frame_lines(&three_and_five, 0, 7, 7);
        assert_eq!(
            last,
            [G(Yesterday), T(3), T(4), T(5), T(6), T(7)],
            "six lines in seven rows: every thread on screen keeps its group row above it"
        );
        assert!(
            last.contains(&T(7)),
            "the cursor on the last thread is shown"
        );
    }
}
