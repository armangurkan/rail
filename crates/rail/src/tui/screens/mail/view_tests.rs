//! The mail screen drawn: the list, the sidebar, the header and the status bar, at 90 columns
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
    let mut screen = MailScreen::new(ctx, work_and_home());
    let effects = screen.start();
    settle(&mut screen, effects).await;
    (tmp, screen)
}

fn draw(screen: &MailScreen, width: u16, height: u16) -> String {
    snapshot(width, height, |frame| screen.view(frame, &Theme::default()))
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
    let area = ratatui::layout::Rect::new(0, 0, 150, 40);
    let areas = layout::areas(area, true, true);
    assert_eq!(areas.header.height, 1);
    assert_eq!(areas.status.height, 1);
    assert_eq!(areas.sidebar.unwrap().width, layout::SIDEBAR_WIDTH);
    assert!(areas.reader.unwrap().width > areas.list.width);
    let narrow = layout::areas(ratatui::layout::Rect::new(0, 0, 90, 24), false, false);
    assert!(narrow.sidebar.is_none() && narrow.reader.is_none());
    assert_eq!(narrow.list.width, 90);
}
