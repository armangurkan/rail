//! The palette's matching, help from the keymap, the filter line's keys, and their drawings.

use crossterm::event::KeyCode;

use super::*;
use crate::tui::data::ScopeChoice;
use crate::tui::keys::{Action, Keymap, View};
use crate::tui::test_util::{key, snapshot};
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
    let help = Help::new(&overridden_keymap(), &[View::List, View::Global]);
    let text = help.text();
    assert!(
        text.contains("Thread list") && text.contains("Everywhere"),
        "{text}"
    );
    assert!(
        text.contains("f *") && text.contains("Filter the list as you type"),
        "{text}"
    );
    assert!(text.contains("* set in config.toml"), "{text}");
    assert!(
        !text.contains("reader.follow_link") && !text.contains("Open a link"),
        "{text}"
    );
    let reader = Help::new(&Keymap::defaults(), &[View::Reader, View::Global]).text();
    assert!(
        reader.contains("g x") && reader.contains("Open a link"),
        "{reader}"
    );
    assert!(!reader.contains('*'), "nothing overridden: {reader}");
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
            Modal::Help(Help::new(&overridden_keymap(), &[View::List, View::Global])),
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
            Modal::Errors(ErrorList::new(vec![
                "11:40  the connection to the daemon closed".into(),
                "11:39  Rail does not open this link: javascript:alert(1)".into(),
            ])),
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
