//! The header on its own: the width rules, how it is drawn, and what the screen puts in it.

use std::sync::Arc;

use rail_core::paths::Paths;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Style};

use super::header::*;
use super::{MailScreen, Summary};
use rail_core::config::Icons;

use crate::tui::data::FakeDaemon;
use crate::tui::test_util::{mail_ctx, render_lines, work_and_home};
use crate::tui::text::width;
use crate::tui::theme::Theme;

fn header(left: &str, slot: Slot, right: &str) -> Header {
    Header {
        left: left.to_string(),
        selected: None,
        slot,
        right: Summary {
            text: right.to_string(),
            error: false,
        },
        clock: None,
        icons: Icons::Unicode,
    }
}

#[test]
fn thousands_separators() {
    assert_eq!(thousands(0), "0");
    assert_eq!(thousands(999), "999");
    assert_eq!(thousands(1_000), "1,000");
    assert_eq!(thousands(22_853), "22,853");
    assert_eq!(thousands(1_234_567), "1,234,567");
    let h = Header {
        selected: Some(1_234),
        ..header("▍Rail │ All inboxes", Slot::None, "x")
    };
    assert_eq!(h.joined(), "▍Rail │ All inboxes · 1,234 selected");
}

#[test]
fn a_wide_header_has_the_slot_between_left_and_right() {
    let h = header(
        "▍Rail │ All inboxes",
        Slot::Idle("/ Search".into()),
        "● Synced 11:40",
    );
    let cells = header_cells(&h, 80);
    assert_eq!(cells.left, "▍Rail │ All inboxes");
    assert_eq!(cells.prefix, "│ 🔍 ", "the search region's prefix");
    let (at, text) = cells.slot.unwrap();
    assert_eq!(
        at,
        width("▍Rail │ All inboxes") + 2 + width("│ 🔍 "),
        "the slot starts after the prefix"
    );
    assert_eq!(text, "/ Search");
    assert_eq!(cells.right.as_deref(), Some("● Synced 11:40"));
}

#[test]
fn under_sixty_columns_the_slot_goes_and_under_forty_the_right_part_goes() {
    let h = header(
        "▍Rail │ All inboxes",
        Slot::Idle("/ Search".into()),
        "● Synced 11:40",
    );
    let cells = header_cells(&h, 59);
    assert!(cells.slot.is_none());
    assert_eq!(cells.right.as_deref(), Some("● Synced 11:40"));
    let long = header(
        &"y".repeat(50),
        Slot::Idle("/ Search".into()),
        "● Synced 11:40",
    );
    let cells = header_cells(&long, 59);
    assert!(cells.slot.is_none());
    assert_eq!(
        cells.right.as_deref(),
        Some("● Synced 11:40"),
        "the right stays whole"
    );
    assert_eq!(
        width(&cells.left),
        59 - 14 - 2,
        "the left is cut to what is left"
    );
    assert!(cells.left.ends_with('…'), "{:?}", cells.left);
    let cells = header_cells(&h, 39);
    assert!(cells.slot.is_none());
    assert!(cells.right.is_none());
    assert_eq!(cells.left, "▍Rail │ All inboxes");
    let wide = header(&"y".repeat(40), Slot::None, "● Synced 11:40");
    let cells = header_cells(&wide, 30);
    assert_eq!(width(&cells.left), 30, "the left part is cut to the width");
}

#[test]
fn the_summary_outranks_the_scope_title_when_both_do_not_fit() {
    let h = Header {
        selected: Some(3),
        ..header(
            "▍Rail │ All inboxes · 2 accounts · 22,853 unread",
            Slot::Idle("/ Search".into()),
            "daemon reachable again (Ctrl-r resumes live updates)",
        )
    };
    let right = "daemon reachable again (Ctrl-r resumes live updates)";
    let cells = header_cells(&h, 90);
    assert_eq!(
        cells.right.as_deref(),
        Some(right),
        "the summary is kept whole"
    );
    assert_eq!(width(&cells.left), 90 - width(right) - 2);
    assert!(cells.left.starts_with("▍Rail │"), "{:?}", cells.left);
    assert!(cells.left.ends_with('…'), "{:?}", cells.left);
    assert!(cells.slot.is_none(), "no room is left for the slot");
    let cells = header_cells(&h, 45);
    assert_eq!(
        width(cells.right.as_deref().unwrap()),
        45,
        "a summary wider than the row is cut"
    );
    assert_eq!(cells.left, "");
}

#[test]
fn a_long_filter_is_cut_from_the_right_and_a_tiny_slot_is_dropped() {
    let long = "x".repeat(100);
    let h = header("▍Rail │ All inboxes", Slot::Active(long), "● Synced 11:40");
    let (_, text) = header_cells(&h, 80).slot.unwrap();
    assert!(text.ends_with('…'));
    assert_eq!(
        width(&text),
        80 - width("▍Rail │ All inboxes") - width("● Synced 11:40") - 4 - width("│ 🔍 ")
    );
    let h = header(
        &"y".repeat(40),
        Slot::Idle("/ Search".into()),
        "● Synced 11:40",
    );
    assert!(
        header_cells(&h, 64).slot.is_none(),
        "under 8 cells the slot is dropped"
    );
}

#[test]
fn no_slot_leaves_the_middle_empty() {
    let h = header("▍Rail │ work · Drafts", Slot::None, "● Synced 11:40");
    let cells = header_cells(&h, 80);
    assert!(cells.slot.is_none());
    assert_eq!(cells.prefix, "", "no search region without a slot");
    assert_eq!(cells.right.as_deref(), Some("● Synced 11:40"));
}

fn bare_screen() -> (tempfile::TempDir, MailScreen) {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = mail_ctx(&Paths::all_in(tmp.path()), Arc::new(FakeDaemon::default()));
    (tmp, MailScreen::new(ctx, work_and_home()))
}

#[test]
fn the_screen_builds_its_header_from_its_state() {
    let (_tmp, screen) = bare_screen();
    assert_eq!(
        screen.header_left(),
        "▍Rail │ All inboxes · 2 accounts · 0 unread"
    );
    assert_eq!(screen.header_summary(), "connecting to the daemon…");
    assert_eq!(screen.slot_text(), "/ Search");
    assert_eq!(screen.header().slot, Slot::Idle("/ Search".into()));
}

#[test]
fn the_header_is_drawn_with_the_slot_between_left_and_right() {
    let h = header(
        "▍Rail │ All inboxes",
        Slot::Idle("/ Search".into()),
        "● Synced 11:40",
    );
    let theme = Theme::default();
    let mut slot = None;
    let lines = render_lines(80, 1, |frame| {
        slot = render_header(frame, frame.area(), &theme, &h);
    });
    let row = &lines[0];
    // The cell after the two-cell magnifier reads as a space here.
    assert!(
        row.starts_with("▍Rail │ All inboxes  │ 🔍  / Search"),
        "{row:?}"
    );
    assert!(row.trim_end().ends_with("● Synced 11:40"), "{row:?}");
    let slot = slot.expect("the slot is drawn at 80 columns");
    let (buffer, _) = drawn(80, 1, &theme, &h);
    assert_eq!(column_of(&buffer, 0, "/"), Some(26));
    assert_eq!((slot.x, slot.y, slot.height), (26, 0, 1));
    let wide = render_lines(30, 1, |frame| {
        assert!(render_header(frame, frame.area(), &theme, &h).is_none());
    });
    assert_eq!(wide[0].trim_end(), "▍Rail │ All inboxes");
}

/// Draw `h` on a `w` x `h` test terminal: the buffer and the slot's rect.
fn drawn(
    cols: u16,
    rows: u16,
    theme: &Theme,
    h: &Header,
) -> (ratatui::buffer::Buffer, Option<ratatui::layout::Rect>) {
    let mut terminal = Terminal::new(TestBackend::new(cols, rows)).unwrap();
    let mut slot = None;
    terminal
        .draw(|frame| {
            slot = render_header(frame, frame.area(), theme, h);
        })
        .unwrap();
    (terminal.backend().buffer().clone(), slot)
}

/// The first column on row `y` whose cell is `symbol`.
fn column_of(buffer: &ratatui::buffer::Buffer, y: u16, symbol: &str) -> Option<u16> {
    (0..buffer.area.width).find(|&x| buffer[(x, y)].symbol() == symbol)
}

#[test]
fn ascii_icons_draw_the_search_region_with_a_bar() {
    let h = Header {
        icons: Icons::Ascii,
        ..header(
            "|Rail | All inboxes",
            Slot::Idle("/ Search".into()),
            "ok Synced 11:40",
        )
    };
    let lines = render_lines(80, 1, |frame| {
        render_header(frame, frame.area(), &Theme::default(), &h);
    });
    assert!(
        lines[0].starts_with("|Rail | All inboxes  | / Search"),
        "{:?}",
        lines[0]
    );
}

#[test]
fn the_selection_count_is_drawn_in_the_text_style_not_the_accent() {
    let h = Header {
        selected: Some(3),
        ..header("▍Rail │ All inboxes", Slot::None, "● Synced 11:40")
    };
    let theme = Theme {
        text: Style::new().fg(Color::Green),
        ..Theme::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
    terminal
        .draw(|frame| {
            render_header(frame, frame.area(), &theme, &h);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let row: String = (0..80).map(|x| buffer[(x, 0)].symbol()).collect();
    let byte = row.find("selected").unwrap();
    let at = u16::try_from(row[..byte].chars().count()).unwrap();
    assert_eq!(buffer[(at, 0)].symbol(), "s");
    assert_eq!(buffer[(at, 0)].fg, Color::Green);
    assert_eq!(buffer[(0, 0)].fg, theme.accent.fg.unwrap());
}

#[test]
fn a_cut_scope_ends_in_an_ellipsis_in_the_text_style() {
    let h = header(&"y".repeat(60), Slot::None, "● Synced 11:40");
    let theme = Theme {
        text: Style::new().fg(Color::Green),
        ..Theme::default()
    };
    let mut terminal = Terminal::new(TestBackend::new(50, 1)).unwrap();
    terminal
        .draw(|frame| {
            render_header(frame, frame.area(), &theme, &h);
        })
        .unwrap();
    let buffer = terminal.backend().buffer();
    let at = u16::try_from(50 - 14 - 2 - 1).unwrap();
    assert_eq!(buffer[(at, 0)].symbol(), "…");
    assert_eq!(buffer[(at, 0)].fg, Color::Green);
    assert_eq!(buffer[(0, 0)].fg, theme.accent.fg.unwrap());
    let tiny = render_lines(40, 1, |frame| {
        render_header(
            frame,
            frame.area(),
            &theme,
            &header("x", Slot::None, &"z".repeat(60)),
        );
    });
    assert_eq!(
        width(tiny[0].trim_end()),
        40,
        "a summary alone too wide fills the row"
    );
}

/// Review Focus 1 (the label half).
#[test]
fn a_label_names_the_account_on_screen_and_a_blank_one_falls_back_to_the_name() {
    let mut config = crate::tui::test_util::existing_config();
    config.accounts[0].label = Some("Personal".into());
    let mut second = config.accounts[0].clone();
    second.name = "home".into();
    second.address = "h@example.com".into();
    second.label = Some("  ".into());
    config.accounts.push(second);
    let mut accounts = crate::tui::screens::mail::AccountEntry::from_config(&config);
    assert_eq!(accounts[0].name, "work", "the name stays the identifier");
    assert_eq!(accounts[0].label, "Personal");
    assert_eq!(accounts[1].label, "home", "a blank label is no label");
    accounts[1].label = "Ev\u{7}il".into();
    let tmp = tempfile::tempdir().unwrap();
    let ctx = mail_ctx(&Paths::all_in(tmp.path()), Arc::new(FakeDaemon::default()));
    let mut screen = MailScreen::new(ctx, accounts);
    screen.scope = crate::tui::data::ScopeChoice::Account(0);
    assert_eq!(screen.header_left(), "▍Rail │ Personal · Inbox · 0 unread");
    screen.scope = crate::tui::data::ScopeChoice::Account(1);
    assert_eq!(
        screen.header_left(),
        "▍Rail │ Ev\\u{7}il · Inbox · 0 unread"
    );
    let bar = screen.bar(&Theme::default());
    assert!(
        bar.right[0].1.starts_with("Personal"),
        "{:?}",
        bar.right[0].1
    );
    assert!(
        bar.right[1].1.starts_with("Ev\\u{7}il"),
        "{:?}",
        bar.right[1].1
    );
    let texts: Vec<String> = screen
        .palette_entries()
        .into_iter()
        .map(|e| e.text)
        .collect();
    assert!(
        texts.contains(&"Go to Personal's inbox".to_string()),
        "{texts:?}"
    );
}

/// `header` with the clock reading `12:39`.
fn clocked(left: &str, slot: Slot, right: &str) -> Header {
    Header {
        clock: Some("12:39".into()),
        ..header(left, slot, right)
    }
}

/// The summary and the clock as one text, as they are drawn.
fn right_unit(cells: &HeaderCells) -> String {
    format!(
        "{}{}",
        cells.right.as_deref().unwrap_or_default(),
        cells.clock.as_deref().unwrap_or_default()
    )
}

#[test]
fn the_clock_is_kept_whole_with_the_summary_at_the_right_edge() {
    let h = clocked(
        "▍Rail │ All inboxes",
        Slot::Idle("/ Search".into()),
        "● Synced 11:40",
    );
    let cells = header_cells(&h, 80);
    assert_eq!(right_unit(&cells), "● Synced 11:40 │ 12:39");
    let (at, text) = cells.slot.clone().unwrap();
    assert_eq!(at, width("▍Rail │ All inboxes") + 2 + width("│ 🔍 "));
    assert_eq!(text, "/ Search");
    let cells = header_cells(&h, 59);
    assert!(cells.clock.is_none(), "under 60 columns the clock goes");
    assert!(cells.slot.is_none());
    assert_eq!(right_unit(&cells), "● Synced 11:40");
    let cells = header_cells(&h, 40);
    assert!(cells.clock.is_none());
    assert_eq!(cells.right.as_deref(), Some("● Synced 11:40"));
    let cells = header_cells(&h, 39);
    assert!(cells.clock.is_none());
    assert!(cells.right.is_none());
    assert_eq!(cells.left, "▍Rail │ All inboxes");
}

#[test]
fn a_long_scope_is_cut_so_the_summary_and_the_clock_stay_whole() {
    let h = clocked(
        &"y".repeat(70),
        Slot::Idle("/ Search".into()),
        "● Synced 11:40",
    );
    let cells = header_cells(&h, 80);
    assert_eq!(right_unit(&cells), "● Synced 11:40 │ 12:39");
    assert_eq!(width(&cells.left), 80 - width("● Synced 11:40 │ 12:39") - 2);
    assert!(cells.left.ends_with('…'), "{:?}", cells.left);
    assert!(cells.slot.is_none());
    let none = clocked("▍Rail │ work · Drafts", Slot::None, "● Synced 11:40");
    let cells = header_cells(&none, 80);
    assert_eq!(cells.prefix, "");
    assert_eq!(right_unit(&cells), "● Synced 11:40 │ 12:39");
}

#[test]
fn the_clock_is_drawn_muted_after_an_error_summary() {
    let h = Header {
        right: Summary {
            text: "! work sync failed".into(),
            error: true,
        },
        ..clocked("▍Rail │ All inboxes", Slot::Idle("/ Search".into()), "")
    };
    let theme = Theme {
        error: Style::new().fg(Color::Red),
        muted: Style::new().fg(Color::DarkGray),
        ..Theme::default()
    };
    let lines = render_lines(80, 1, |frame| {
        render_header(frame, frame.area(), &theme, &h);
    });
    assert!(
        lines[0].ends_with("! work sync failed │ 12:39"),
        "{:?}",
        lines[0]
    );
    let (buffer, slot) = drawn(80, 1, &theme, &h);
    assert_eq!(buffer[(79, 0)].symbol(), "9");
    assert_eq!(buffer[(79, 0)].fg, Color::DarkGray, "the clock is muted");
    let bang = column_of(&buffer, 0, "!").unwrap();
    assert_eq!(
        buffer[(bang, 0)].fg,
        Color::Red,
        "the summary keeps its error"
    );
    let slot = slot.unwrap();
    assert_eq!(column_of(&buffer, 0, "/"), Some(slot.x));
}
