//! The thread list's rows: the dot, the account's badge (merged mailboxes only), the people,
//! `★` and `📎`, the subject with its count after it, and the time right-aligned at the edge.
//! People, subjects and a failed action's text are outside text: `pad` and `fit` escape them.
#![cfg_attr(
    not(test),
    expect(dead_code, reason = "used from the wiring task on; remove then")
)]

use rail_core::store::view::ThreadRow;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::view::{Glyphs, participants, people_cols};
use crate::tui::text::{fit, pad, pad_left, width};
use crate::tui::theme::Theme;

/// Columns of the time at the right edge (`2024-10-03`).
pub(super) const TIME_COLS: usize = 10;
/// The dot and its space.
const DOT_COLS: usize = 2;
/// The two spaces after the people, and the two before the time.
const GAP_COLS: usize = 2;
/// `★` (or a space), `📎` (two cells, or two spaces) and a space.
const MARKS_COLS: usize = 4;

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
        + TIME_COLS;
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
    let time = pad_left(&ctx.time, TIME_COLS);
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

    use super::*;

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
}
