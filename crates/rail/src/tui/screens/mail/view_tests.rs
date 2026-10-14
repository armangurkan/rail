//! The mail screen drawn: the header, the sidebar, the list and the hint bar, at 90 columns
//! (no sidebar, stacked) and 150 (sidebar, split), with Unicode glyphs and with ASCII.

use std::sync::Arc;

use rail_core::config::Icons;
use rail_core::ops::SyncPhase;
use rail_core::paths::Paths;
use rail_core::provider::{AttachmentMeta, Body, Mailbox};
use rail_core::sync::Batch;
use ratatui::style::Modifier;

use super::*;
use crate::tui::data::FakeDaemon;
use crate::tui::screen::Screen;
use crate::tui::test_util::{
    BASE_MS, mail_ctx, mail_envelope, seed_store, settle, snapshot, status, work_and_home,
};
use crate::tui::theme::Theme;

/// A small mailbox with every kind of row: unread and read, starred, with an attachment, a
/// three-message thread, a long subject, a hostile one, and mail for both accounts.
async fn screen(icons: Icons) -> (tempfile::TempDir, MailScreen) {
    screen_with(icons, true).await
}

/// `screen` with `hints = false` when `hints` is false.
async fn screen_with(icons: Icons, hints: bool) -> (tempfile::TempDir, MailScreen) {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let hour = 3_600_000;
    let mut dana = mail_envelope("w1", "wt1", BASE_MS + 26 * hour);
    dana.subject = "Q4 roadmap review — can we move to Thursday?".into();
    dana.from = vec![Mailbox {
        name: Some("Dana Reyes".into()),
        address: "dana@example.com".into(),
    }];
    dana.is_starred = true;
    let mut tom = mail_envelope("w2", "wt1", BASE_MS + 27 * hour);
    tom.subject = dana.subject.clone();
    tom.from = vec![Mailbox {
        name: Some("Tom".into()),
        address: "tom@example.com".into(),
    }];
    let mut dana_again = mail_envelope("w3", "wt1", BASE_MS + 27 * hour + 60_000);
    dana_again.subject = dana.subject.clone();
    dana_again.from = dana.from.clone();
    let mut gitlab = mail_envelope("w4", "wt2", BASE_MS + 25 * hour);
    gitlab.subject = "Pipeline failed: rail!42".into();
    gitlab.from = vec![Mailbox {
        name: Some("GitLab".into()),
        address: "gitlab@example.com".into(),
    }];
    let mut priya = mail_envelope("w5", "wt3", BASE_MS - 20 * hour);
    priya.subject = "Re: offsite logistics".into();
    priya.from = vec![Mailbox {
        name: Some("Priya Nair".into()),
        address: "priya@example.com".into(),
    }];
    priya.is_read = true;
    let mut hostile = mail_envelope("w6", "wt4", BASE_MS - 30 * 24 * hour);
    hostile.subject = "pay\u{202e}fdp.exe \u{1b}[2J urgent".into();
    hostile.from = vec![Mailbox {
        name: Some("Evil\u{7}Corp".into()),
        address: "e@example.com".into(),
    }];
    let mut long = mail_envelope("w7", "wt5", BASE_MS - 400 * 24 * hour);
    long.subject =
        "A very long subject that cannot possibly fit on one row of a terminal ".repeat(4);
    let home = mail_envelope("h1", "ht1", BASE_MS + 2 * hour);
    let envelopes = vec![
        (0, dana),
        (0, tom),
        (0, dana_again),
        (0, gitlab),
        (0, priya),
        (0, hostile),
        (0, long),
        (1, home),
    ];
    let mut store = seed_store(
        &paths,
        &[("work", "w@example.com"), ("home", "h@example.com")],
        &envelopes,
    );
    let pdf = Body {
        text: "plan".into(),
        attachments: vec![AttachmentMeta {
            part_id: "1".into(),
            filename: "plan.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 2048,
            plain_file: true,
        }],
        ..Body::default()
    };
    store
        .apply(
            &Batch {
                bodies: vec![("w4".into(), pdf)],
                ..Batch::new(1)
            },
            BASE_MS / 1000,
        )
        .unwrap();
    let fake = Arc::new(FakeDaemon::default());
    let mut backfill = status("home", "h@example.com", SyncPhase::Backfill);
    backfill.backfill_fetched = 1_200;
    backfill.backfill_estimate = Some(5_000);
    *fake.statuses.lock().unwrap() =
        vec![status("work", "w@example.com", SyncPhase::Idle), backfill];
    let mut ctx = mail_ctx(&paths, fake);
    ctx.icons = icons;
    ctx.hints = hints;
    let mut screen = MailScreen::new(ctx, work_and_home());
    let effects = screen.start();
    settle(&mut screen, effects).await;
    (tmp, screen)
}

fn draw(screen: &MailScreen, width: u16, height: u16) -> String {
    snapshot(width, height, |frame| screen.view(frame, &Theme::default()))
}

async fn press(screen: &mut MailScreen, code: crossterm::event::KeyCode) {
    let effects = screen.update(crate::tui::app::Msg::Key(crate::tui::test_util::key(code)));
    settle(screen, effects).await;
}

/// Move the sidebar's cursor down to the first row labelled `label`.
async fn sidebar_to(screen: &mut MailScreen, label: &str) {
    for _ in 0..screen.sidebar.rows().len() {
        if screen.sidebar.selected().is_some_and(|r| r.label == label) {
            return;
        }
        press(screen, crossterm::event::KeyCode::Char('j')).await;
    }
    panic!("no sidebar row {label:?}");
}

/// `screen` with `work`'s folders as a tree: Trash, Projects, `Clients` with `Clients/Acme` (one
/// unread thread), a Gmail category and a folder with a hostile name.
async fn tree_screen(icons: Icons) -> (tempfile::TempDir, MailScreen) {
    use rail_core::provider::{FolderKind, RemoteFolder};
    let (tmp, mut screen) = screen(icons).await;
    let folder = |id: &str, name: &str, kind: FolderKind| RemoteFolder {
        parent: None,
        remote_id: id.into(),
        name: name.into(),
        kind,
    };
    let folders = vec![
        folder("INBOX", "Inbox", FolderKind::Inbox),
        folder("SENT", "Sent", FolderKind::Sent),
        folder("TRASH", "Trash", FolderKind::Trash),
        folder("Label_1", "Projects", FolderKind::User),
        folder("Label_2", "Clients", FolderKind::User),
        folder("Label_3", "Clients/Acme", FolderKind::User),
        folder("CATEGORY_SOCIAL", "Category: Social", FolderKind::Label),
        folder("Label_4", "Ev\u{1b}[2Jil", FolderKind::User),
    ];
    let mut acme = mail_envelope("wc", "wtc", BASE_MS + 3_600_000);
    acme.folders = vec!["Label_3".into()];
    let batch = Batch {
        folders: Some(folders),
        upserts: vec![acme],
        ..Batch::new(1)
    };
    let db = Paths::all_in(tmp.path()).db_file();
    let mut store = rail_core::store::Store::open(&db).unwrap();
    store.apply(&batch, BASE_MS / 1000).unwrap();
    let effect = screen.read_sidebar();
    settle(&mut screen, vec![effect]).await;
    (tmp, screen)
}

#[tokio::test]
async fn the_sidebar_drawn_folded_and_unfolded() {
    use crossterm::event::KeyCode;
    for (icons, name) in [(Icons::Unicode, "unicode"), (Icons::Ascii, "ascii")] {
        let (_tmp, mut screen) = tree_screen(icons).await;
        let _ = draw(&screen, 90, 24);
        press(&mut screen, KeyCode::Char('b')).await;
        insta::assert_snapshot!(format!("sidebar_shown_{name}_90x24"), draw(&screen, 90, 24));
        press(&mut screen, KeyCode::Tab).await;
        sidebar_to(&mut screen, "More folders (5)").await;
        press(&mut screen, KeyCode::Char(' ')).await;
        sidebar_to(&mut screen, "Clients").await;
        press(&mut screen, KeyCode::Char(' ')).await;
        press(&mut screen, KeyCode::Tab).await;
        insta::assert_snapshot!(
            format!("sidebar_unfolded_{name}_150x40"),
            draw(&screen, 150, 40)
        );
    }
}

/// Review Focus 1 (the folder half, on screen).
#[tokio::test]
async fn a_hostile_folder_name_is_escaped_in_the_sidebar() {
    use crossterm::event::KeyCode;
    let (_tmp, mut screen) = tree_screen(Icons::Unicode).await;
    press(&mut screen, KeyCode::Tab).await;
    sidebar_to(&mut screen, "More folders (5)").await;
    press(&mut screen, KeyCode::Char(' ')).await;
    let lines =
        crate::tui::test_util::render_lines(150, 40, |frame| screen.view(frame, &Theme::default()));
    assert!(lines.iter().all(|l| !l.contains('\u{1b}')));
    assert!(
        lines.iter().any(|l| l.contains("Ev\\u{1b}[2Jil")),
        "{lines:#?}"
    );
}

/// Review Focus 5 (the drawn half).
#[tokio::test]
async fn the_badge_takes_the_account_colour_and_the_current_scope_row_the_accent() {
    use ratatui::style::Color;
    let (_tmp, mut screen) = screen(Icons::Unicode).await;
    press(&mut screen, crossterm::event::KeyCode::Char('1')).await;
    let theme = Theme::default();
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(150, 40)).unwrap();
    terminal.draw(|frame| screen.view(frame, &theme)).unwrap();
    let buffer = terminal.backend().buffer().clone();
    let side = |y: u16| {
        (1..25u16)
            .map(|x| buffer[(x, y)].symbol())
            .collect::<String>()
    };
    let work = (0..40u16)
        .find(|&y| side(y).contains(" W  work"))
        .expect("work's account row");
    let x = (1..25u16)
        .find(|&x| buffer[(x, work)].symbol() == "W")
        .unwrap();
    assert_eq!(buffer[(x, work)].bg, Color::Cyan, "work's colour");
    assert_eq!(buffer[(x, work)].fg, Color::Black);
    assert_eq!(
        buffer[(x - 1, work)].bg,
        Color::Cyan,
        "one cell of padding each side"
    );
    assert_eq!(buffer[(x + 1, work)].bg, Color::Cyan);
    let inbox = work + 1;
    assert!(side(inbox).contains("Inbox"), "{}", side(inbox));
    let accent = theme.accent.fg.unwrap();
    assert_eq!(
        buffer[(8, inbox)].fg,
        accent,
        "work's Inbox is the current row"
    );
    assert_ne!(buffer[(20, work)].fg, accent, "its account row is not");
}

/// The sidebar fits in a 40-row terminal with `More folders` folded and scrolls otherwise: the
/// cursor stays in view while the sidebar has focus, the current scope's row while it has not.
#[tokio::test]
async fn the_sidebar_scrolls_to_keep_the_cursor_or_the_current_row_in_view() {
    let (_tmp, mut screen) = screen(Icons::Unicode).await;
    press(&mut screen, crossterm::event::KeyCode::Tab).await;
    for _ in 0..30 {
        press(&mut screen, crossterm::event::KeyCode::Char('j')).await;
    }
    let lines =
        crate::tui::test_util::render_lines(150, 14, |frame| screen.view(frame, &Theme::default()));
    let side: Vec<String> = lines.iter().map(|l| l.chars().take(26).collect()).collect();
    assert!(
        side.iter().any(|l| l.contains("More folders (1)")),
        "the last row is in view: {side:#?}"
    );
    assert!(
        !side.iter().any(|l| l.contains("UNIFIED")),
        "the top scrolled away: {side:#?}"
    );
    // The list takes the keys; the scope is still All inboxes, at the top: it scrolls back.
    press(&mut screen, crossterm::event::KeyCode::Tab).await;
    assert_eq!(screen.focus, Focus::List);
    let lines =
        crate::tui::test_util::render_lines(150, 14, |frame| screen.view(frame, &Theme::default()));
    let side: Vec<String> = lines.iter().map(|l| l.chars().take(26).collect()).collect();
    assert!(
        side.iter().any(|l| l.contains("All inboxes")),
        "the current scope's row is in view without focus: {side:#?}"
    );
}

#[tokio::test]
async fn mail_screen_snapshots() {
    for (icons, name) in [(Icons::Unicode, "unicode"), (Icons::Ascii, "ascii")] {
        let (_tmp, screen) = screen(icons).await;
        for (width, height) in [(90, 24), (150, 40)] {
            insta::assert_snapshot!(
                format!("mail_list_{name}_{width}x{height}"),
                draw(&screen, width, height)
            );
        }
    }
}

/// At 120 columns the sidebar shows by default and, below the split, sits unboxed beside its
/// right rule.
#[tokio::test]
async fn the_sidebar_unboxed_at_120_columns() {
    let (_tmp, screen) = screen(Icons::Unicode).await;
    let _ = draw(&screen, 120, 30);
    assert!(screen.sidebar_visible(), "shown by default at 120 columns");
    insta::assert_snapshot!("mail_sidebar_unboxed_120x30", draw(&screen, 120, 30));
}

#[tokio::test]
async fn the_sidebar_focused_and_a_label_scope() {
    let (_tmp, mut screen) = screen(Icons::Unicode).await;
    let effects = screen.update(crate::tui::app::Msg::Key(crate::tui::test_util::key(
        crossterm::event::KeyCode::Tab,
    )));
    settle(&mut screen, effects).await;
    insta::assert_snapshot!("mail_sidebar_focused_150x40", draw(&screen, 150, 40));
}

#[tokio::test]
async fn unread_rows_are_bold_and_the_selected_row_is_highlighted() {
    let (_tmp, screen) = screen(Icons::Unicode).await;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(90, 24)).unwrap();
    terminal
        .draw(|frame| screen.view(frame, &Theme::default()))
        .unwrap();
    let buffer = terminal.backend().buffer().clone();
    let row_of = |needle: &str| {
        (0..24u16)
            .find(|y| {
                (0..90u16)
                    .map(|x| buffer[(x, *y)].symbol())
                    .collect::<String>()
                    .contains(needle)
            })
            .unwrap()
    };
    let unread = row_of("GitLab");
    let read = row_of("Priya Nair");
    assert!(buffer[(20, unread)].modifier.contains(Modifier::BOLD));
    assert!(!buffer[(20, read)].modifier.contains(Modifier::BOLD));
    let selected = row_of("Q4 roadmap");
    assert_eq!(
        buffer[(20, selected)].bg,
        Theme::default().selected.bg.unwrap()
    );
}

#[test]
fn layout_pins_the_split_threshold_and_the_sidebar_rule() {
    assert!(!layout::is_split(139, 140));
    assert!(layout::is_split(140, 140));
    assert!(!layout::sidebar_by_default(99));
    assert!(layout::sidebar_by_default(100));
    assert_eq!(layout::bordered(140, 140), layout::is_split(140, 140));
    assert!(!layout::bordered(139, 140), "borders only in split mode");
    let area = ratatui::layout::Rect::new(0, 0, 150, 40);
    let areas = layout::areas(area, true, true, true);
    assert_eq!(areas.header.height, 1);
    assert_eq!(areas.hints.height, layout::HINT_ROWS_BOXED);
    assert_eq!(
        areas.hints.height, 3,
        "split mode boxes each key on three rows"
    );
    assert_eq!(areas.body.height, 36);
    assert_eq!(areas.body.y, 1);
    assert_eq!(areas.sidebar.unwrap().width, layout::SIDEBAR_WIDTH);
    assert!(areas.reader.unwrap().width > areas.list.width);
    let narrow = layout::areas(ratatui::layout::Rect::new(0, 0, 90, 24), false, false, true);
    assert!(narrow.sidebar.is_none() && narrow.reader.is_none());
    assert_eq!(narrow.list.width, 90);
    assert_eq!(narrow.hints.height, 1, "stacked mode keeps the one-row bar");
    let short = layout::areas(ratatui::layout::Rect::new(0, 0, 150, 10), true, true, true);
    assert_eq!(short.hints.height, 1, "too short for key boxes");
    let under = layout::areas(
        ratatui::layout::Rect::new(0, 0, 150, layout::MIN_ROWS_FOR_KEY_BOXES - 1),
        true,
        true,
        true,
    );
    assert_eq!(under.hints.height, 1);
    let just = layout::areas(
        ratatui::layout::Rect::new(0, 0, 150, layout::MIN_ROWS_FOR_KEY_BOXES),
        true,
        true,
        true,
    );
    assert_eq!(just.hints.height, 3);
    let off = layout::areas(area, true, true, false);
    assert_eq!(off.hints.height, 1, "hints off: one row in split mode too");
    assert_eq!(off.body.height, 38, "the boxes' rows go to the panes");
    assert!(layout::fits_border(ratatui::layout::Rect::new(0, 0, 4, 3)));
    assert!(!layout::fits_border(ratatui::layout::Rect::new(0, 0, 3, 3)));
    assert!(!layout::fits_border(ratatui::layout::Rect::new(0, 0, 4, 2)));
    let theme = Theme::default();
    assert_eq!(
        layout::pane_block(&theme, Icons::Unicode, true).inner(area),
        ratatui::layout::Rect::new(1, 1, 148, 38),
        "a boxed pane loses one cell on each side"
    );
    let boxed = |icons| {
        crate::tui::test_util::render_lines(4, 3, |frame| {
            frame.render_widget(layout::pane_block(&theme, icons, false), frame.area());
        })
    };
    assert_eq!(boxed(Icons::Ascii), ["+--+", "|  |", "+--+"], "ASCII boxes");
    assert_eq!(boxed(Icons::Unicode), ["┌──┐", "│  │", "└──┘"]);
}

#[tokio::test]
async fn the_frame_boxes_the_panes_in_split_mode_and_marks_the_focused_one() {
    let (_tmp, mut screen) = screen(Icons::Unicode).await;
    let theme = Theme::default();
    let buffer_of = |screen: &MailScreen| {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(150, 40)).unwrap();
        terminal.draw(|frame| screen.view(frame, &theme)).unwrap();
        terminal.backend().buffer().clone()
    };
    // List focused: its top-left corner is at column 26 (after the 26-column sidebar).
    let buffer = buffer_of(&screen);
    assert_eq!(
        buffer[(0, 1)].symbol(),
        "┌",
        "the sidebar's box starts under the header"
    );
    assert_eq!(buffer[(0, 1)].fg, theme.border.fg.unwrap());
    assert_eq!(buffer[(26, 1)].symbol(), "┌");
    assert_eq!(
        buffer[(26, 1)].fg,
        theme.border_focus.fg.unwrap(),
        "the list has focus"
    );
    assert_eq!(
        buffer[(0, 36)].symbol(),
        "└",
        "the sidebar's box ends above the hint rows"
    );
    let lines = crate::tui::test_util::render_lines(150, 40, |frame| screen.view(frame, &theme));
    assert!(lines[37].starts_with('┌'), "{}", lines[37]);
    assert!(lines[38].starts_with("│ Enter │ open"), "{}", lines[38]);
    assert!(lines[39].starts_with('└'), "{}", lines[39]);
    assert!(
        lines[38]
            .trim_end()
            .ends_with("● work ✓ · ● home ⟳ 1200/5000"),
        "{}",
        lines[38]
    );
    assert!(
        !lines[36].contains("Enter"),
        "no frame around the bar: {}",
        lines[36]
    );
    // Sidebar focused.
    let effects = screen.update(crate::tui::app::Msg::Key(crate::tui::test_util::key(
        crossterm::event::KeyCode::Tab,
    )));
    settle(&mut screen, effects).await;
    let buffer = buffer_of(&screen);
    assert_eq!(buffer[(0, 1)].fg, theme.border_focus.fg.unwrap());
    assert_eq!(buffer[(26, 1)].fg, theme.border.fg.unwrap());
}

#[tokio::test]
async fn the_header_and_the_hint_bar_are_on_every_screen() {
    let (_tmp, mut screen) = screen(Icons::Unicode).await;
    let rows = |screen: &MailScreen, w, h| {
        crate::tui::test_util::render_lines(w, h, |frame| screen.view(frame, &Theme::default()))
    };
    let top = rows(&screen, 90, 24)[0].clone();
    assert!(
        top.starts_with("▍Rail │ All inboxes · 2 accounts · "),
        "{top}"
    );
    assert!(top.contains("/ Search"), "{top}");
    assert!(top.trim_end().ends_with("⟳ Backfill 1200/5000"), "{top}");
    let bottom = rows(&screen, 90, 24)[23].clone();
    assert!(bottom.starts_with("Enter open  j/k move"), "{bottom}");
    assert!(
        !bottom.contains("Tab sidebar"),
        "no sidebar at 90 columns: {bottom}"
    );
    assert!(bottom.contains("? help"), "{bottom}");
    assert!(
        bottom.trim_end().ends_with("● work ✓ · ● home ⟳ 1200/5000"),
        "{bottom}"
    );
    // Stacked reader: header above, hints below.
    let effects = screen.update(crate::tui::app::Msg::Key(crate::tui::test_util::key(
        crossterm::event::KeyCode::Enter,
    )));
    settle(&mut screen, effects).await;
    let lines = rows(&screen, 90, 24);
    assert!(lines[0].starts_with("▍Rail │"), "{}", lines[0]);
    assert!(
        lines[1].contains("(3 messages)"),
        "the reader's title is the first body row"
    );
    assert!(
        lines[23].starts_with("n/p message  Tab fold"),
        "{}",
        lines[23]
    );
}

#[tokio::test]
async fn a_notice_replaces_the_hints_and_the_hints_return() {
    let (_tmp, mut screen) = screen(Icons::Unicode).await;
    screen.status.notify("draft kept", screen.tick);
    let rows = |screen: &MailScreen| {
        crate::tui::test_util::render_lines(90, 24, |frame| screen.view(frame, &Theme::default()))
    };
    let bottom = rows(&screen)[23].clone();
    assert!(bottom.starts_with("draft kept"), "{bottom}");
    assert!(!bottom.contains("? help"), "{bottom}");
    for _ in 0..=status::NOTICE_TICKS {
        let effects = screen.update(crate::tui::app::Msg::Tick);
        settle(&mut screen, effects).await;
    }
    let bottom = rows(&screen)[23].clone();
    assert!(bottom.starts_with("Enter open"), "{bottom}");
    // A notice wider than the room is cut; the account states stay whole on the right.
    screen.status.notify("x".repeat(200), screen.tick);
    let bottom = rows(&screen)[23].clone();
    assert!(bottom.starts_with("xxxx"), "{bottom}");
    assert!(
        bottom.trim_end().ends_with("● work ✓ · ● home ⟳ 1200/5000"),
        "{bottom}"
    );
}

#[tokio::test]
async fn with_hints_off_the_bar_is_one_row_of_notices_and_account_states() {
    let (_tmp, screen) = screen_with(Icons::Unicode, false).await;
    let lines =
        crate::tui::test_util::render_lines(150, 40, |frame| screen.view(frame, &Theme::default()));
    let states = "● work ✓ · ● home ⟳ 1200/5000";
    assert_eq!(lines[39].trim(), states, "only the accounts: {}", lines[39]);
    assert!(lines[39].trim_end().ends_with(states), "{}", lines[39]);
    assert!(lines[39].starts_with(' '), "right-aligned: {}", lines[39]);
    assert!(!lines.iter().any(|l| l.contains("? help")), "no hints");
    for row in [37, 38] {
        assert!(
            !lines[row].contains("Enter"),
            "no key boxes: {}",
            lines[row]
        );
    }
    assert!(lines[37].starts_with('│'), "a pane row: {}", lines[37]);
    assert!(
        lines[38].starts_with('└'),
        "the sidebar's box ends just above the bar: {}",
        lines[38]
    );
    insta::assert_snapshot!("mail_list_hints_off_150x40", draw(&screen, 150, 40));
}

#[tokio::test]
async fn with_hints_off_a_notice_shows_on_the_left_of_the_last_row() {
    let (_tmp, mut screen) = screen_with(Icons::Unicode, false).await;
    screen.status.notify("draft kept", screen.tick);
    let lines =
        crate::tui::test_util::render_lines(150, 40, |frame| screen.view(frame, &Theme::default()));
    assert!(lines[39].starts_with("draft kept"), "{}", lines[39]);
    assert!(
        lines[39]
            .trim_end()
            .ends_with("● work ✓ · ● home ⟳ 1200/5000"),
        "{}",
        lines[39]
    );
}

#[test]
fn a_pane_too_small_for_a_border_is_drawn_without_one() {
    // Covered through the layout rule; the view skips the box when `fits_border` is false.
    let tiny = ratatui::layout::Rect::new(0, 0, 150, 2);
    assert!(!layout::fits_border(tiny));
}

#[tokio::test]
async fn a_two_row_body_in_split_mode_has_no_borders() {
    let (_tmp, screen) = screen(Icons::Unicode).await;
    let lines =
        crate::tui::test_util::render_lines(150, 4, |frame| screen.view(frame, &Theme::default()));
    assert!(!lines[1].contains('┌'), "{}", lines[1]);
    assert!(
        lines[1].contains("All inboxes") || lines[1].contains("Q4 roadmap"),
        "{}",
        lines[1]
    );
}

#[tokio::test]
async fn ascii_icons_draw_the_stacked_sidebar_rule_with_a_bar() {
    let (_tmp, screen) = screen(Icons::Ascii).await;
    let lines =
        crate::tui::test_util::render_lines(120, 30, |frame| screen.view(frame, &Theme::default()));
    let rule = |line: &str| line.chars().nth(usize::from(layout::SIDEBAR_WIDTH) - 1);
    assert_eq!(rule(&lines[1]), Some('|'), "{}", lines[1]);
    assert!(lines[0].starts_with("|Rail | All inboxes"), "{}", lines[0]);
}

#[tokio::test]
async fn the_filter_is_typed_in_the_header_slot_and_falls_back_to_the_hint_row_when_narrow() {
    let (_tmp, mut screen) = screen(Icons::Unicode).await;
    let press = |screen: &mut MailScreen, code| {
        screen.update(crate::tui::app::Msg::Key(crate::tui::test_util::key(code)))
    };
    let _ = press(&mut screen, crossterm::event::KeyCode::Char('/'));
    for c in "is:unread".chars() {
        let _ = press(&mut screen, crossterm::event::KeyCode::Char(c));
    }
    let rows = |screen: &MailScreen, w, h| {
        crate::tui::test_util::render_lines(w, h, |frame| screen.view(frame, &Theme::default()))
    };
    let top = rows(&screen, 90, 24)[0].clone();
    assert!(top.contains("/is:unread"), "typed in the slot: {top}");
    let bottom = rows(&screen, 90, 24)[23].clone();
    assert!(
        bottom.starts_with("Enter open"),
        "the hints stay while typing: {bottom}"
    );
    let narrow = rows(&screen, 50, 24);
    assert!(
        !narrow[0].contains("/is:unread"),
        "no slot under 60 columns: {}",
        narrow[0]
    );
    assert!(
        narrow[23].starts_with("/is:unread"),
        "so the line takes the hint row: {}",
        narrow[23]
    );
    // The filter runs when its debounce timer fires.
    let event = crate::tui::data::MailEvent::Timer {
        token: screen.filter_token,
        kind: crate::tui::data::Timer::Filter,
    };
    let effects = screen.update(crate::tui::app::Msg::Task(
        crate::tui::app::TaskResult::Mail(event),
    ));
    settle(&mut screen, effects).await;
    let effects = press(&mut screen, crossterm::event::KeyCode::Enter);
    settle(&mut screen, effects).await;
    assert_eq!(screen.slot_text(), "/ is:unread");
    let top = rows(&screen, 90, 24)[0].clone();
    assert!(
        top.contains("/ is:unread"),
        "active filter in the slot: {top}"
    );
}
