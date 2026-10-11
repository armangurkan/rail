//! The palette's matching, help from the keymap, the filter line's keys, and their drawings.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rail_core::config::Icons;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Position;

use super::*;
use crate::tui::data::ScopeChoice;
use crate::tui::keys::{Action, Keymap, View};
use crate::tui::test_util::{key, render_lines, snapshot};
use crate::tui::theme::Theme;

fn entries() -> Vec<PaletteEntry> {
    let entry = |text: &str, id: &str, key: &str, target: Target| PaletteEntry {
        text: text.into(),
        search: format!("{text} {id}"),
        key: key.into(),
        target,
    };
    vec![
        entry(
            "Filter the list as you type",
            "list.filter",
            "/",
            Target::Action(Action::ListFilter),
        ),
        entry(
            "Show or hide the sidebar",
            "list.sidebar",
            "b",
            Target::Action(Action::ListSidebar),
        ),
        entry("Show recent errors", "errors", "", Target::Errors),
        entry(
            "Go to home's inbox",
            "account home",
            "2",
            Target::Scope(ScopeChoice::Account(1)),
        ),
        entry(
            "Label: work · Projects",
            "label",
            "",
            Target::Scope(ScopeChoice::Folder {
                account: 0,
                folder_id: 3,
                name: "Projects".into(),
            }),
        ),
    ]
}

/// The default keys with `list.filter` moved to `f`, so help shows the override marker.
fn overridden_keymap() -> Keymap {
    let text = "[keys]\n\"list.filter\" = \"f\"\n";
    let path = std::path::Path::new("/c/config.toml");
    let config = rail_core::config::parse(text, path).unwrap();
    Keymap::from_config(&config.keys, text, path).unwrap()
}

fn typed(palette: &mut Palette, text: &str) {
    for c in text.chars() {
        assert_eq!(palette.key(key(KeyCode::Char(c))), ModalOutcome::Stay);
    }
}

#[test]
fn the_palette_matches_fuzzily_and_runs_the_best_entry() {
    let mut palette = Palette::new(entries());
    assert_eq!(palette.shown().len(), 5, "everything before typing");
    typed(&mut palette, "errors");
    assert_eq!(palette.shown()[0].target, Target::Errors);
    assert_eq!(
        palette.key(key(KeyCode::Enter)),
        ModalOutcome::Run(Command::Palette(Target::Errors))
    );
    let mut palette = Palette::new(entries());
    typed(&mut palette, "proj");
    assert!(matches!(
        palette.shown()[0].target,
        Target::Scope(ScopeChoice::Folder { .. })
    ));
    let mut palette = Palette::new(entries());
    typed(&mut palette, "zzzz");
    assert!(palette.shown().is_empty());
    assert_eq!(
        palette.key(key(KeyCode::Enter)),
        ModalOutcome::Stay,
        "nothing to run"
    );
    assert_eq!(palette.key(key(KeyCode::Esc)), ModalOutcome::Close);
}

#[test]
fn arrows_move_through_the_matches() {
    let mut palette = Palette::new(entries());
    palette.key(key(KeyCode::Down));
    palette.key(key(KeyCode::Down));
    assert_eq!(
        palette.key(key(KeyCode::Enter)),
        ModalOutcome::Run(Command::Palette(Target::Errors))
    );
}

#[test]
fn help_lists_the_view_and_the_global_keys_and_marks_overrides() {
    let help = Help::new(
        &overridden_keymap(),
        &[View::List, View::Global],
        Icons::Unicode,
    );
    let text = help.text();
    assert!(
        text.contains("Thread list") && text.contains("Everywhere"),
        "{text}"
    );
    assert!(
        text.contains("f *") && text.contains("Filter the list as you type"),
        "{text}"
    );
    assert_eq!(help.title(), "Keys (* set in config.toml)");
    assert!(
        !text.contains("reader.follow_link") && !text.contains("Open a link"),
        "{text}"
    );
    let reader = Help::new(
        &Keymap::defaults(),
        &[View::Reader, View::Global],
        Icons::Unicode,
    )
    .text();
    assert!(
        reader.contains("g x") && reader.contains("Open a link"),
        "{reader}"
    );
    assert!(!reader.contains('*'), "nothing overridden: {reader}");
    assert_eq!(
        Help::new(
            &Keymap::defaults(),
            &[View::Reader, View::Global],
            Icons::Unicode
        )
        .title(),
        "Keys"
    );
}

#[test]
fn the_filter_line_reports_each_change_and_esc_clears() {
    let mut line = FilterLine::new("");
    assert_eq!(
        line.key(key(KeyCode::Char('i'))),
        ModalOutcome::Typed("i".into())
    );
    for c in "s:unread form:dana".chars() {
        line.key(key(KeyCode::Char(c)));
    }
    assert_eq!(line.parsed(), "is:unread · words: form:dana");
    assert_eq!(line.key(key(KeyCode::Enter)), ModalOutcome::Close);
    assert_eq!(
        line.key(key(KeyCode::Esc)),
        ModalOutcome::Run(Command::ClearFilter)
    );
    assert_eq!(
        line.key(key(KeyCode::Backspace)),
        ModalOutcome::Typed("is:unread form:dan".into())
    );
}

#[test]
fn the_filter_line_stops_at_the_length_limit() {
    let mut line = FilterLine::new(&"x".repeat(rail_core::store::filter::MAX_FILTER_CHARS));
    assert_eq!(line.key(key(KeyCode::Char('y'))), ModalOutcome::Stay);
    assert_eq!(
        line.key(key(KeyCode::Backspace)),
        ModalOutcome::Typed("x".repeat(rail_core::store::filter::MAX_FILTER_CHARS - 1))
    );
}

#[test]
fn modal_snapshots() {
    let mut palette = Palette::new(entries());
    typed(&mut palette, "go");
    let modals = [
        ("palette", Modal::Palette(palette)),
        (
            "help",
            Modal::Help(Help::new(
                &overridden_keymap(),
                &[View::List, View::Global],
                Icons::Unicode,
            )),
        ),
        ("filter", {
            let mut line = FilterLine::new("");
            for c in "is:unread form:dana".chars() {
                line.key(key(KeyCode::Char(c)));
            }
            Modal::Filter(line)
        }),
        (
            "errors",
            Modal::Errors(ErrorList::new(
                vec![
                    "11:40  the connection to the daemon closed".into(),
                    "11:39  Rail does not open this link: javascript:alert(1)".into(),
                ],
                Icons::Unicode,
            )),
        ),
    ];
    for (name, modal) in &modals {
        for (width, height) in [(90, 24), (150, 40)] {
            let text = snapshot(width, height, |frame| {
                modal.render(frame, frame.area(), &Theme::default());
            });
            insta::assert_snapshot!(format!("modal_{name}_{width}x{height}"), text);
        }
    }
}

#[test]
fn a_confirmation_taller_than_the_screen_keeps_its_answer_line_and_cuts_with_an_ellipsis() {
    let lines = (0..30).map(|n| format!("line {n}")).collect();
    let confirm = Confirm::new("Open?", lines, Command::ClearFilter);
    for rows in [3u16, 4, 8, 12] {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, rows)).unwrap();
        terminal
            .draw(|f| confirm.render(f, f.area(), &Theme::default()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let row =
            |y: u16| -> String { (0..40).map(|x| buffer[(x, y)].symbol()).collect::<String>() };
        let last = rows - 2;
        assert!(row(last).contains("y yes"), "{rows}: {}", row(last));
        if rows >= 4 {
            assert!(
                row(last - 1).trim_end_matches(['│', ' ']).ends_with('…'),
                "{rows}: {}",
                row(last - 1)
            );
        }
    }
}

fn keymap_with(text: &str) -> Keymap {
    let path = std::path::Path::new("/c/config.toml");
    let config = rail_core::config::parse(text, path).unwrap();
    Keymap::from_config(&config.keys, text, path).unwrap()
}

#[test]
fn the_scroll_window_hints_only_at_hidden_rows() {
    use super::scroll::{Window, window};
    let w = |first, rows, above, below| Window {
        first,
        rows,
        above,
        below,
    };
    assert_eq!(window(5, 0, 10), w(0, 5, false, false), "everything fits");
    assert_eq!(window(30, 0, 10), w(0, 9, false, true));
    assert_eq!(window(30, 5, 10), w(6, 8, true, true));
    assert_eq!(
        window(30, 99, 10),
        w(21, 9, true, false),
        "clamped to the last page"
    );
    assert_eq!(
        window(30, 99, 2),
        w(28, 2, false, false),
        "no room for hints"
    );
    assert_eq!(window(0, 3, 0), w(0, 0, false, false));
}

#[test]
fn every_scroll_step_down_reveals_a_new_row() {
    use super::scroll::window;
    let (total, height) = (30, 10);
    let bottom = |offset| {
        let w = window(total, offset, height);
        w.first + w.rows - 1
    };
    let end = window(total, 1_000, height).first;
    let mut offset = 0;
    while window(total, offset, height).first < end {
        assert!(
            bottom(offset + 1) > bottom(offset),
            "offset {offset} to {} shows no new row",
            offset + 1
        );
        offset += 1;
    }
    assert_eq!(
        bottom(offset),
        total - 1,
        "the last step ends at the last row"
    );
    assert_eq!(window(total, 0, height).first, 0);
    assert_eq!(
        window(total, 1, height).first,
        2,
        "the hint takes row 1's place"
    );
}

#[test]
fn help_hints_at_hidden_rows_and_keeps_the_legend_in_its_title() {
    let mut help = Help::new(
        &overridden_keymap(),
        &[View::List, View::Global],
        Icons::Unicode,
    );
    let draw = |help: &Help| render_lines(90, 24, |f| help.render(f, f.area(), &Theme::default()));
    // At 90x24 the box spans rows 1-22, so its inside is rows 2-21.
    let rows = draw(&help);
    assert!(
        rows[1].contains("Keys (* set in config.toml)"),
        "{}",
        rows[1]
    );
    assert!(rows[21].contains("↓ more"), "{}", rows[21]);
    assert!(!rows.iter().any(|r| r.contains("↑ more")));
    for _ in 0..40 {
        help.key(key(KeyCode::Char('j')));
    }
    let rows = draw(&help);
    assert!(rows[2].contains("↑ more"), "{}", rows[2]);
    assert!(
        rows[21].contains("In the palette"),
        "the last page ends at the last row"
    );
    assert!(
        rows[1].contains("* set in config.toml"),
        "the legend scrolls with nothing"
    );
    help.key(key(KeyCode::Up));
    assert!(draw(&help)[21].contains("↓ more"));
    let ascii = Help::new(
        &overridden_keymap(),
        &[View::List, View::Global],
        Icons::Ascii,
    );
    assert!(draw(&ascii)[21].contains("v more"));
}

#[test]
fn help_closes_on_its_bound_key_esc_and_q() {
    let mut help = Help::new(
        &keymap_with("[keys]\n\"global.help\" = \"f2\"\n"),
        &[View::List, View::Global],
        Icons::Unicode,
    );
    assert_eq!(
        help.key(key(KeyCode::Char('?'))),
        ModalOutcome::Stay,
        "? is not the help key now"
    );
    assert_eq!(help.key(key(KeyCode::F(2))), ModalOutcome::Close);
    assert_eq!(help.key(key(KeyCode::Esc)), ModalOutcome::Close);
    assert_eq!(help.key(key(KeyCode::Char('q'))), ModalOutcome::Close);
    let mut default = Help::new(&Keymap::defaults(), &[View::Global], Icons::Unicode);
    let shifted = KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT);
    assert_eq!(
        default.key(shifted),
        ModalOutcome::Close,
        "? as terminals send it"
    );
}

#[test]
fn errors_scroll_with_the_help_keys_and_hint_at_hidden_rows() {
    let rows = (0..40)
        .map(|n| format!("11:{n:02}  error number {n}"))
        .collect();
    let mut list = ErrorList::new(rows, Icons::Ascii);
    let draw =
        |list: &ErrorList| render_lines(90, 24, |f| list.render(f, f.area(), &Theme::default()));
    let shows = |rows: &[String], n: usize| {
        rows.iter()
            .any(|r| r.contains(&format!("error number {n} ")))
    };
    // At 90x24 the box spans rows 2-21, so its inside is rows 3-20.
    let rows = draw(&list);
    assert!(shows(&rows, 0) && shows(&rows, 16) && !shows(&rows, 17));
    assert!(rows[20].contains("v more"), "{}", rows[20]);
    assert_eq!(list.key(key(KeyCode::Char('j'))), ModalOutcome::Stay);
    let rows = draw(&list);
    assert!(rows[3].contains("^ more"), "{}", rows[3]);
    assert!(!shows(&rows, 0) && !shows(&rows, 1) && shows(&rows, 2) && shows(&rows, 17));
    list.key(key(KeyCode::Down));
    list.key(key(KeyCode::Char('k')));
    list.key(key(KeyCode::Up));
    assert!(shows(&draw(&list), 0), "back at the top");
    assert_eq!(list.key(key(KeyCode::Char('q'))), ModalOutcome::Close);
}

/// Where the terminal cursor is after drawing `modal` at 90x24 (`None`: hidden).
fn cursor_after(modal: &Modal) -> Option<Position> {
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal
        .draw(|frame| modal.render(frame, frame.area(), &Theme::default()))
        .unwrap();
    let backend = terminal.backend();
    backend.cursor_visible().then(|| backend.cursor_position())
}

#[test]
fn the_palette_and_the_filter_line_draw_the_cursor_where_it_is() {
    let mut palette = Palette::new(entries());
    typed(&mut palette, "日本");
    let mut modal = Modal::Palette(palette);
    // At 90x24 the palette's inside starts at (11, 5); `:` takes column 11.
    assert_eq!(cursor_after(&modal), Some(Position::new(16, 5)));
    modal.key(key(KeyCode::Left));
    assert_eq!(
        cursor_after(&modal),
        Some(Position::new(14, 5)),
        "a wide character back"
    );
    modal.key(key(KeyCode::Home));
    assert_eq!(cursor_after(&modal), Some(Position::new(12, 5)));
    modal.key(key(KeyCode::Right));
    assert_eq!(cursor_after(&modal), Some(Position::new(14, 5)));
    // The filter line is the bottom row; `/` takes column 0.
    let mut line = Modal::Filter(FilterLine::new("a\u{1b}b"));
    assert_eq!(
        cursor_after(&line),
        Some(Position::new(9, 23)),
        "after the escape's columns"
    );
    line.key(key(KeyCode::Left));
    line.key(key(KeyCode::Left));
    assert_eq!(cursor_after(&line), Some(Position::new(2, 23)));
    line.key(key(KeyCode::End));
    assert_eq!(cursor_after(&line), Some(Position::new(9, 23)));
}

#[test]
fn errors_with_a_retry_move_a_highlight_and_enter_retries_it() {
    let rows = vec![
        ErrorRow {
            text: "Could not archive: gone".into(),
            retry: Some(7),
        },
        ErrorRow {
            text: "The store could not be read".into(),
            retry: None,
        },
    ];
    let mut list = ErrorList::with_retries(rows, Icons::Ascii);
    assert_eq!(
        list.key(key(KeyCode::Enter)),
        ModalOutcome::Run(Command::Retry(7))
    );
    assert_eq!(list.key(key(KeyCode::Char('j'))), ModalOutcome::Stay);
    assert_eq!(
        list.key(key(KeyCode::Enter)),
        ModalOutcome::Close,
        "not retryable"
    );
}
