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
        dim_read: DimRead::Dot,
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

mod dim_read;
