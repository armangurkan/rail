//! The reader inside the mail screen: opening, expansion, `n`/`p`/`j`/`k`, fetching and the
//! re-read, per-message errors, a lost daemon, folding, links (`g x`, OSC 8, refused links),
//! the attachment list, pane versus full screen, and snapshots.

use std::rc::Rc;
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use rail_core::config::Icons;
use rail_core::ops::{
    BodyFetchResponse, BodyFetchResult, BodyOutcome, ErrorCode, OpError, SyncPhase,
};
use rail_core::paths::Paths;
use rail_core::provider::{AttachmentMeta, Body, Mailbox};
use rail_core::store::Store;
use rail_core::sync::Batch;
use url::Url;

use crate::tui::app::{Msg, TaskResult};
use crate::tui::data::{DaemonEvent, FakeCall, FakeDaemon, MailEvent};
use crate::tui::screen::Screen;
use crate::tui::screens::mail::{Focus, MailScreen};
use crate::tui::test_util::{
    BASE_MS, ctrl, key, mail_ctx, mail_envelope, recording_opener, seed_store, settle, snapshot,
    status, work_and_home,
};
use crate::tui::theme::Theme;

mod cards;
mod links;
mod open;
mod state;

const BODY: &str = "Hi Dana,\n\nThe plan is in the doc[1]; notes are on the wiki[2].\n\n> On Tue, Tom wrote:\n> one\n> two\n> three\n> four\n\nThanks,\nTom";

struct Fixture {
    _tmp: tempfile::TempDir,
    paths: Paths,
    fake: Arc<FakeDaemon>,
    opened: Arc<Mutex<Vec<Url>>>,
    screen: MailScreen,
}

/// One work thread of three messages: m1 read with `BODY` (two links, a quote, a PDF), m2 read
/// and not fetched, m3 unread and not fetched (the latest).
async fn fixture(hyperlinks: bool) -> Fixture {
    fixture_with(hyperlinks, Icons::Unicode).await
}

/// `fixture` with `icons`.
async fn fixture_with(hyperlinks: bool, icons: Icons) -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let mut m1 = mail_envelope("m1", "t1", BASE_MS);
    m1.from = vec![Mailbox {
        name: Some("Tom".into()),
        address: "tom@example.com".into(),
    }];
    m1.to = vec![Mailbox {
        name: Some("Dana Reyes".into()),
        address: "dana@example.com".into(),
    }];
    m1.is_read = true;
    m1.subject = "Q4 plan".into();
    let mut m2 = mail_envelope("m2", "t1", BASE_MS + 60_000);
    m2.is_read = true;
    m2.subject = "Q4 plan".into();
    let mut m3 = mail_envelope("m3", "t1", BASE_MS + 120_000);
    m3.subject = "Q4 plan".into();
    let mut store = seed_store(
        &paths,
        &[("work", "w@example.com")],
        &[(0, m1), (0, m2), (0, m3)],
    );
    let body = Body {
        text: BODY.into(),
        html: Some("<p>Hi Dana</p>".into()),
        links: vec![
            "https://docs.example/plan".into(),
            "https://wiki.example/notes".into(),
        ],
        attachments: vec![AttachmentMeta {
            part_id: "2".into(),
            filename: "plan.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 2_048,
            plain_file: true,
        }],
        ..Body::default()
    };
    store
        .apply(
            &Batch {
                bodies: vec![("m1".into(), body)],
                ..Batch::new(1)
            },
            BASE_MS / 1000,
        )
        .unwrap();
    let fake = Arc::new(FakeDaemon::default());
    *fake.statuses.lock().unwrap() = vec![status("work", "w@example.com", SyncPhase::Idle)];
    let (opener, opened) = recording_opener();
    let mut ctx = mail_ctx(&paths, fake.clone());
    ctx.opener = opener;
    ctx.hyperlinks = hyperlinks;
    ctx.icons = icons;
    let mut screen = MailScreen::new(ctx, work_and_home()[..1].to_vec());
    let effects = screen.start();
    settle(&mut screen, effects).await;
    Fixture {
        _tmp: tmp,
        paths,
        fake,
        opened,
        screen,
    }
}

async fn press(f: &mut Fixture, code: KeyCode) {
    let effects = f.screen.update(Msg::Key(key(code)));
    settle(&mut f.screen, effects).await;
}

async fn keys(f: &mut Fixture, text: &str) {
    for c in text.chars() {
        press(f, KeyCode::Char(c)).await;
    }
}

/// Store `body of <id>` for each id, as the daemon does before it answers.
fn write_bodies(paths: &Paths, ids: &[String]) {
    let bodies = ids
        .iter()
        .map(|id| {
            (
                id.clone(),
                Body {
                    text: format!("body of {id}"),
                    ..Body::default()
                },
            )
        })
        .collect();
    Store::open(&paths.db_file())
        .unwrap()
        .apply(
            &Batch {
                bodies,
                ..Batch::new(1)
            },
            BASE_MS / 1000,
        )
        .unwrap();
}

/// The fake daemon stores the bodies it is asked for before it answers, as the real one does.
fn store_bodies_on_fetch(f: &Fixture) {
    let paths = f.paths.clone();
    *f.fake.on_fetch.lock().unwrap() =
        Some(Box::new(move |ids: &[String]| write_bodies(&paths, ids)));
}

/// Move the reader's cursor down until its line contains `needle` (at most 200 lines).
async fn cursor_to(f: &mut Fixture, needle: &str) {
    for _ in 0..200 {
        if f.screen
            .reader_cursor_text()
            .is_some_and(|t| t.contains(needle))
        {
            return;
        }
        press(f, KeyCode::Char('j')).await;
    }
    panic!("no reader line contains {needle:?}");
}

/// Put the cursor on m1's card (Tom's, the first card oldest first) and open it.
async fn open_toms_card(f: &mut Fixture) {
    press(f, KeyCode::Home).await;
    press(f, KeyCode::Char('n')).await;
    press(f, KeyCode::Tab).await;
}

/// Every reader line at a width where nothing wraps, joined.
fn all_text(f: &Fixture) -> String {
    let lines = f.screen.reader().unwrap().lines(400);
    lines
        .iter()
        .map(|l| l.text().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn fetch_calls(f: &Fixture) -> Vec<FakeCall> {
    f.fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|c| matches!(c, FakeCall::Fetch { .. }))
        .cloned()
        .collect()
}

fn draw(f: &Fixture, width: u16, height: u16) -> String {
    snapshot(width, height, |frame| {
        f.screen.view(frame, &Theme::default())
    })
}

#[tokio::test]
async fn opening_fetches_every_uncached_body_once_and_shows_fetching_until_it_lands() {
    let mut f = fixture(false).await;
    // The daemon's first answer covers no id yet: both bodies are still on their way.
    *f.fake.answer.lock().unwrap() = Some(Ok(BodyFetchResponse {
        results: Vec::new(),
    }));
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(f.screen.focus, Focus::Reader);
    assert_eq!(
        fetch_calls(&f),
        [FakeCall::Fetch {
            account: "work".into(),
            ids: vec!["m2".into(), "m3".into()]
        }],
        "one body.fetch for both uncached messages"
    );
    let text = draw(&f, 90, 24);
    assert!(text.contains("fetching…"), "{text}");
    write_bodies(&f.paths, &["m2".into(), "m3".into()]);
    let answer = BodyFetchResponse {
        results: ["m2", "m3"]
            .map(|id| BodyFetchResult {
                message_id: id.into(),
                outcome: BodyOutcome::Fetched,
            })
            .to_vec(),
    };
    let event = MailEvent::Bodies {
        thread_id: 1,
        result: Ok(answer),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
    let text = draw(&f, 90, 24);
    assert!(
        text.contains("body of m3"),
        "the latest is expanded: {text}"
    );
    assert!(!text.contains("fetching…"), "{text}");
    assert_eq!(fetch_calls(&f).len(), 1, "nothing asked twice");
}

/// A daemon that says "fetched" while the store still has no body must not make the reader
/// ask again and again.
#[tokio::test]
async fn a_body_that_never_lands_is_not_asked_for_again() {
    let mut f = fixture(false).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(fetch_calls(&f).len(), 1);
    assert!(
        all_text(&f).contains("The body did not arrive"),
        "{}",
        all_text(&f)
    );
    press(&mut f, KeyCode::Char('q')).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(
        fetch_calls(&f).len(),
        2,
        "opening the thread again tries again"
    );
}

#[tokio::test]
async fn the_newest_opens_under_the_title_block_and_n_p_tab_move_through_the_cards() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    let text = draw(&f, 90, 40);
    assert!(
        text.contains("Sender  Thu 08:55   W  WORK"),
        "the title block: {text}"
    );
    assert!(text.contains("/  3 messages"), "{text}");
    assert!(text.contains("body of m3"), "the newest is open: {text}");
    assert!(text.contains("▸   1   Tom"), "m1 is a card: {text}");
    assert!(
        text.contains("        snippet m1"),
        "its preview under the sender: {text}"
    );
    assert!(!text.contains("The plan is in the doc"), "{text}");
    assert_eq!(
        f.screen.reader_cursor_text().as_deref(),
        Some("Q4 plan"),
        "the cursor starts on the subject"
    );
    press(&mut f, KeyCode::Char('n')).await;
    assert!(
        f.screen
            .reader_cursor_text()
            .unwrap()
            .starts_with("▸   1   Tom")
    );
    press(&mut f, KeyCode::Tab).await;
    let text = draw(&f, 90, 40);
    assert!(text.contains("▾   1   Tom"), "{text}");
    assert!(text.contains("The plan is in the doc[1]"), "{text}");
    assert!(text.contains("[5 quoted lines]"), "{text}");
    assert!(text.contains("plan.pdf"), "the attachment line: {text}");
    press(&mut f, KeyCode::Char('n')).await;
    assert!(
        f.screen
            .reader_cursor_text()
            .unwrap()
            .starts_with("▸   2   Sender")
    );
    press(&mut f, KeyCode::Char('p')).await;
    press(&mut f, KeyCode::Char('p')).await;
    assert_eq!(f.screen.reader_cursor_text().as_deref(), Some("Q4 plan"));
}

#[tokio::test]
async fn enter_opens_and_closes_a_fold() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    open_toms_card(&mut f).await;
    cursor_to(&mut f, "quoted lines").await;
    press(&mut f, KeyCode::Enter).await;
    let text = draw(&f, 90, 40);
    assert!(
        text.contains("> On Tue, Tom wrote:") && text.contains("> four"),
        "{text}"
    );
    press(&mut f, KeyCode::Enter).await;
    assert!(draw(&f, 90, 40).contains("[5 quoted lines]"));
}

#[tokio::test]
async fn a_failed_fetch_shows_on_its_message_with_outside_text_escaped() {
    let mut f = fixture(false).await;
    let failure = OpError::new(
        ErrorCode::Network,
        rail_core::error::ErrorClass::Transient,
        "the mail provider did not answer",
    )
    .with_detail("timeout \u{1b}[2J");
    *f.fake.answer.lock().unwrap() = Some(Ok(BodyFetchResponse {
        results: vec![
            BodyFetchResult {
                message_id: "m2".into(),
                outcome: BodyOutcome::Missing,
            },
            BodyFetchResult {
                message_id: "m3".into(),
                outcome: BodyOutcome::Failed(failure),
            },
        ],
    }));
    press(&mut f, KeyCode::Enter).await;
    let text = all_text(&f);
    assert!(
        text.contains("Could not fetch this message: the mail provider did not answer"),
        "{text}"
    );
    assert!(
        text.contains("from outside Rail (the provider or the system): \"timeout \\u{1b}[2J\""),
        "{text}"
    );
    assert!(!draw(&f, 120, 40).contains('\u{1b}'));
    assert!(!text.contains("fetching…"));
    open_toms_card(&mut f).await;
    assert!(
        all_text(&f).contains("The plan is in the doc"),
        "the rest stays readable"
    );
}

/// Review focus 3.
#[tokio::test]
async fn a_fetch_lost_with_the_daemon_shows_an_error_not_fetching_forever() {
    let mut f = fixture(false).await;
    let closed = OpError::new(
        ErrorCode::Daemon,
        rail_core::error::ErrorClass::Transient,
        "the connection to the daemon closed",
    );
    *f.fake.answer.lock().unwrap() = Some(Err(closed.clone()));
    press(&mut f, KeyCode::Enter).await;
    let lost = MailEvent::Daemon(DaemonEvent::Lost(closed));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(lost)));
    settle(&mut f.screen, effects).await;
    let text = all_text(&f);
    assert!(!text.contains("fetching…"), "{text}");
    assert!(
        text.contains("Could not fetch this message: the connection to the daemon closed"),
        "{text}"
    );
    assert!(
        f.screen.header_summary().contains("daemon unreachable"),
        "{}",
        f.screen.header_summary()
    );
    press(&mut f, KeyCode::Char('q')).await;
    let effects = f.screen.update(Msg::Key(ctrl('r')));
    settle(&mut f.screen, effects).await;
    assert_eq!(
        f.fake.calls.lock().unwrap().last(),
        Some(&FakeCall::Watch),
        "re-subscribed"
    );
}

#[tokio::test]
async fn a_login_failure_hints_once_in_the_status_bar() {
    let mut f = fixture(false).await;
    let auth = OpError::new(
        ErrorCode::Auth,
        rail_core::error::ErrorClass::Permanent,
        "this account needs a new login",
    )
    .with_hint("log in again with `rail account login work`");
    let answer = |auth: &OpError| BodyFetchResponse {
        results: ["m2", "m3"]
            .map(|id| BodyFetchResult {
                message_id: id.into(),
                outcome: BodyOutcome::Failed(auth.clone()),
            })
            .to_vec(),
    };
    *f.fake.answer.lock().unwrap() = Some(Ok(answer(&auth)));
    press(&mut f, KeyCode::Enter).await;
    assert!(
        f.screen.status_line().contains("rail account login work"),
        "{}",
        f.screen.status_line()
    );
    let errors_before = f.screen.error_count();
    let event = MailEvent::Bodies {
        thread_id: 1,
        result: Ok(answer(&auth)),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
    assert_eq!(
        f.screen.error_count(),
        errors_before,
        "the hint is shown once per session"
    );
}

#[tokio::test]
async fn g_x_confirms_a_single_link_and_asks_which_of_several() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    open_toms_card(&mut f).await;
    cursor_to(&mut f, "doc[1]").await;
    keys(&mut f, "gx").await;
    let text = draw(&f, 90, 30);
    assert!(
        text.contains("Open link number (1-2)"),
        "two markers on the line: {text}"
    );
    keys(&mut f, "2").await;
    press(&mut f, KeyCode::Enter).await;
    let text = draw(&f, 90, 30);
    assert!(
        text.contains("https://wiki.example/notes"),
        "the address is shown first: {text}"
    );
    assert!(
        f.opened.lock().unwrap().is_empty(),
        "nothing opened before the answer"
    );
    keys(&mut f, "y").await;
    assert_eq!(
        f.opened
            .lock()
            .unwrap()
            .iter()
            .map(Url::to_string)
            .collect::<Vec<_>>(),
        ["https://wiki.example/notes"]
    );
    keys(&mut f, "gx").await;
    press(&mut f, KeyCode::Esc).await;
    assert_eq!(f.opened.lock().unwrap().len(), 1, "Esc cancels");
}

/// Review focus 5.
#[tokio::test]
async fn a_stored_link_rail_would_not_open_is_refused() {
    let mut f = fixture(true).await;
    store_bodies_on_fetch(&f);
    let hostile = r#"["javascript:alert(1)","https://evil.example/\u001b]8;;x\u0007"]"#;
    Store::open(&f.paths.db_file())
        .unwrap()
        .conn()
        .execute(
            "UPDATE messages SET links = ?1 WHERE remote_id = 'm1'",
            [hostile],
        )
        .unwrap();
    press(&mut f, KeyCode::Enter).await;
    open_toms_card(&mut f).await;
    cursor_to(&mut f, "doc[1]").await;
    keys(&mut f, "gx").await;
    keys(&mut f, "1").await;
    press(&mut f, KeyCode::Enter).await;
    assert!(
        f.screen
            .status_line()
            .contains("Rail does not open this link: javascript:alert(1)")
    );
    keys(&mut f, "gx").await;
    keys(&mut f, "2").await;
    press(&mut f, KeyCode::Enter).await;
    assert!(
        f.screen.status_line().contains("\\u{1b}]8;;x\\u{7}"),
        "{}",
        f.screen.status_line()
    );
    assert!(f.opened.lock().unwrap().is_empty());
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| f.screen.view(frame, &Theme::default()))
        .unwrap();
    let cells: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(
        !cells.contains("\u{1b}]8"),
        "no hyperlink for a refused address"
    );
}

#[tokio::test]
async fn markers_become_osc8_hyperlinks_where_the_terminal_supports_them() {
    let mut f = fixture(true).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    open_toms_card(&mut f).await;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40)).unwrap();
    terminal
        .draw(|frame| f.screen.view(frame, &Theme::default()))
        .unwrap();
    let buffer = terminal.backend().buffer();
    let linked: Vec<&ratatui::buffer::Cell> = buffer
        .content()
        .iter()
        .filter(|c| c.symbol().starts_with("\u{1b}]8;;"))
        .collect();
    assert_eq!(linked.len(), 2);
    assert_eq!(
        linked[0].symbol(),
        "\u{1b}]8;;https://docs.example/plan\u{1b}\\[1]\u{1b}]8;;\u{1b}\\"
    );
    assert_eq!(
        linked[0].diff_option,
        ratatui::buffer::CellDiffOption::ForcedWidth(std::num::NonZeroU16::new(3).unwrap())
    );
}

#[tokio::test]
async fn the_reader_is_a_pane_when_wide_and_a_full_screen_view_when_narrow() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    let wide = draw(&f, 150, 40);
    assert!(
        wide.contains("Q4 plan") && wide.contains("All inboxes"),
        "list and reader together: {wide}"
    );
    // Narrow, the reader takes the body: its subject is the first row under the header.
    let narrow = draw(&f, 90, 24);
    assert_eq!(
        narrow.lines().nth(1),
        Some("Q4 plan"),
        "the reader covers the list: {narrow}"
    );
    assert_eq!(narrow.lines().nth(3), Some("/  3 messages"), "{narrow}");
    press(&mut f, KeyCode::Char('q')).await;
    assert_eq!(f.screen.focus, Focus::List);
    let list = draw(&f, 90, 24);
    // The list's group row, then its first thread.
    let body = list.lines().nth(2).unwrap_or_default();
    assert!(
        body.contains("Q4 plan") && !body.contains("3 messages"),
        "the list is back: {list}"
    );
}

#[tokio::test]
async fn lines_are_cached_per_width() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    let reader = f.screen.reader().unwrap();
    let a = reader.lines(80);
    let b = reader.lines(80);
    assert!(Rc::ptr_eq(&a, &b));
    let c = reader.lines(60);
    assert!(!Rc::ptr_eq(&a, &c));
    assert!(c.iter().all(|l| crate::tui::text::width(l.text()) <= 60));
}

#[tokio::test]
async fn a_store_change_that_leaves_the_thread_as_it_was_rebuilds_nothing() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('j')).await;
    press(&mut f, KeyCode::Char('j')).await;
    let before = f.screen.reader().unwrap().lines(80);
    let cursor = f.screen.reader_cursor_text();
    let progress = status("work", "w@example.com", SyncPhase::Delta);
    let event = MailEvent::Daemon(DaemonEvent::Progress(Box::new(progress)));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
    let after = f.screen.reader().unwrap().lines(80);
    assert!(Rc::ptr_eq(&before, &after), "the lines were not rebuilt");
    assert_eq!(f.screen.reader_cursor_text(), cursor, "the cursor stayed");
}

#[tokio::test]
async fn reader_snapshots() {
    for icons in [Icons::Unicode, Icons::Ascii] {
        let mut f = fixture_with(false, icons).await;
        store_bodies_on_fetch(&f);
        press(&mut f, KeyCode::Enter).await;
        // A frame first, as the app draws one before each key: the reader knows its height,
        // so `n` to Tom's card does not scroll the title block away.
        let _ = draw(&f, 90, 24);
        open_toms_card(&mut f).await;
        if icons == Icons::Ascii {
            insta::assert_snapshot!("reader_ascii_90x24", draw(&f, 90, 24));
            continue;
        }
        for (width, height) in [(90, 24), (150, 40)] {
            insta::assert_snapshot!(format!("reader_{width}x{height}"), draw(&f, width, height));
        }
    }
}

/// A thread of 120 uncached messages asks for them in requests of at most 50.
#[tokio::test]
async fn a_long_thread_fetches_in_requests_of_fifty() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let envelopes: Vec<_> = (0..120)
        .map(|i| {
            let mut m = mail_envelope(&format!("m{i:03}"), "t1", BASE_MS + i64::from(i) * 60_000);
            m.is_read = true;
            (0, m)
        })
        .collect();
    seed_store(&paths, &[("work", "w@example.com")], &envelopes);
    let fake = Arc::new(FakeDaemon::default());
    let mut screen = MailScreen::new(
        mail_ctx(&paths, fake.clone()),
        work_and_home()[..1].to_vec(),
    );
    let effects = screen.start();
    settle(&mut screen, effects).await;
    let effects = screen.update(Msg::Key(key(KeyCode::Enter)));
    settle(&mut screen, effects).await;
    let sizes: Vec<usize> = fake
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter_map(|c| match c {
            FakeCall::Fetch { ids, .. } => Some(ids.len()),
            _ => None,
        })
        .collect();
    assert_eq!(sizes, [50, 50, 20]);
}

#[tokio::test]
async fn a_resize_wraps_the_lines_again() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    open_toms_card(&mut f).await;
    let wide = draw(&f, 100, 40);
    assert!(wide.contains("The plan is in the doc[1]; notes are on the wiki[2]."));
    let narrow = draw(&f, 30, 60);
    assert!(!narrow.contains("The plan is in the doc[1]; notes are on the wiki[2]."));
    assert!(narrow.contains("wiki[2]."), "{narrow}");
    assert!(narrow.lines().all(|l| crate::tui::text::width(l) <= 30));
}

/// A `Lost` event fails the pending fetches even when the request itself never answers, and
/// `Ctrl-r` reconnects while the reader still has focus.
#[tokio::test]
async fn a_lost_link_fails_pending_fetches_and_ctrl_r_reconnects_from_the_reader() {
    let mut f = fixture(false).await;
    // The answer covers no id: both bodies stay "fetching…" as if the request never returned.
    *f.fake.answer.lock().unwrap() = Some(Ok(BodyFetchResponse {
        results: Vec::new(),
    }));
    press(&mut f, KeyCode::Enter).await;
    assert!(all_text(&f).contains("fetching…"));
    let closed = OpError::new(
        ErrorCode::Daemon,
        rail_core::error::ErrorClass::Transient,
        "the connection to the daemon closed",
    );
    let lost = MailEvent::Daemon(DaemonEvent::Lost(closed));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(lost)));
    settle(&mut f.screen, effects).await;
    let text = all_text(&f);
    assert!(!text.contains("fetching…"), "{text}");
    assert!(
        text.contains("Could not fetch this message: the connection to the daemon closed"),
        "{text}"
    );
    assert_eq!(f.screen.focus, Focus::Reader);
    let effects = f.screen.update(Msg::Key(ctrl('r')));
    settle(&mut f.screen, effects).await;
    assert_eq!(
        f.fake.calls.lock().unwrap().last(),
        Some(&FakeCall::Watch),
        "re-subscribed with the reader still open"
    );
    assert!(
        !f.screen.header_summary().contains("daemon unreachable"),
        "{}",
        f.screen.header_summary()
    );
    assert_eq!(f.screen.focus, Focus::Reader);
}
