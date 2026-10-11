//! `o`: the original HTML in the browser, from a private temporary file.

use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use rail_core::ops::{BodyFetchResponse, BodyFetchResult, BodyOutcome, OpError};
use rail_core::paths::Paths;
use rail_core::provider::Body;
use rail_core::store::Store;
use rail_core::sync::Batch;
use url::Url;

use super::{Fixture, draw, fixture, keys, press, store_bodies_on_fetch};
use crate::tui::app::{Msg, TaskResult};
use crate::tui::data::{FakeDaemon, MailEvent};
use crate::tui::screen::Screen;
use crate::tui::screens::mail::MailScreen;
use crate::tui::test_util::{BASE_MS, mail_ctx, settle, work_and_home};

fn file_paths(opened: &Mutex<Vec<Url>>) -> Vec<std::path::PathBuf> {
    opened
        .lock()
        .unwrap()
        .iter()
        .map(|u| u.to_file_path().expect("a file URL"))
        .collect()
}

#[tokio::test]
async fn o_confirms_once_names_remote_images_and_removes_the_file_on_close() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Home).await;
    keys(&mut f, "o").await;
    let text = draw(&f, 90, 24);
    assert!(
        text.contains("Remote images in this message will load there"),
        "{text}"
    );
    assert!(
        f.opened.lock().unwrap().is_empty(),
        "nothing before the answer"
    );
    keys(&mut f, "y").await;
    let files = file_paths(&f.opened);
    assert_eq!(files.len(), 1);
    let written = std::fs::read_to_string(&files[0]).unwrap();
    assert!(written.ends_with("<p>Hi Dana</p>"), "{written}");
    assert!(files[0].starts_with(f.paths.cache_dir.join("open")));
    keys(&mut f, "o").await;
    assert_eq!(
        f.opened.lock().unwrap().len(),
        2,
        "no second confirmation this session"
    );
    let files = file_paths(&f.opened);
    press(&mut f, KeyCode::Char('q')).await;
    assert!(
        files.iter().all(|p| !p.exists()),
        "the reader closed: its files are gone"
    );
}

#[tokio::test]
async fn no_at_the_confirmation_writes_and_opens_nothing() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Home).await;
    keys(&mut f, "on").await;
    assert!(f.opened.lock().unwrap().is_empty());
    let dir = f.paths.cache_dir.join("open");
    assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none());
}

#[tokio::test]
async fn o_on_a_message_without_html_says_so() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::End).await;
    keys(&mut f, "o").await;
    keys(&mut f, "y").await;
    assert!(f.opened.lock().unwrap().is_empty());
    assert!(
        f.screen.status_line().contains("has no HTML part"),
        "{}",
        f.screen.status_line()
    );
}

#[tokio::test]
async fn o_on_an_uncached_message_fetches_first_and_opens_when_it_lands() {
    let mut f = fixture(false).await;
    *f.fake.answer.lock().unwrap() = Some(Ok(BodyFetchResponse {
        results: Vec::new(),
    }));
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::End).await;
    keys(&mut f, "o").await;
    assert!(
        f.screen.status_line().contains("fetching"),
        "{}",
        f.screen.status_line()
    );
    let html = Body {
        text: "late".into(),
        html: Some("<p>late</p>".into()),
        ..Body::default()
    };
    Store::open(&f.paths.db_file())
        .unwrap()
        .apply(
            &Batch {
                bodies: vec![("m3".into(), html)],
                ..Batch::new(1)
            },
            BASE_MS / 1000,
        )
        .unwrap();
    let answer = BodyFetchResponse {
        results: vec![BodyFetchResult {
            message_id: "m3".into(),
            outcome: BodyOutcome::Fetched,
        }],
    };
    let event = MailEvent::Bodies {
        thread_id: 1,
        result: Ok(answer),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
    keys(&mut f, "y").await;
    let files = file_paths(&f.opened);
    assert_eq!(files.len(), 1);
    assert!(
        std::fs::read_to_string(&files[0])
            .unwrap()
            .ends_with("<p>late</p>")
    );
}

#[tokio::test]
async fn a_browser_that_will_not_start_is_reported() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    f.screen.set_opener(std::sync::Arc::new(|_: &Url| {
        Err(std::io::Error::other("no browser \u{1b}[2J"))
    }));
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Home).await;
    keys(&mut f, "oy").await;
    let line = f.screen.status_line();
    assert!(
        line.contains("Could not start the browser: no browser \\u{1b}[2J"),
        "{line}"
    );
}

#[tokio::test]
async fn starting_the_screen_sweeps_a_crashs_old_files() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let dir = paths.cache_dir.join("open");
    std::fs::create_dir_all(&dir).unwrap();
    let old = dir.join("old.html");
    std::fs::write(&old, "x").unwrap();
    std::fs::File::options()
        .write(true)
        .open(&old)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 86_400))
        .unwrap();
    let fake = Arc::new(FakeDaemon::default());
    let mut screen = MailScreen::new(mail_ctx(&paths, fake), work_and_home()[..1].to_vec());
    let _ = screen.start();
    assert!(!old.exists());
}

/// Open the thread, ask for m3's HTML while its fetch is out, and answer that fetch with a
/// failure for m3.
async fn open_then_fail_m3(f: &mut Fixture) {
    *f.fake.answer.lock().unwrap() = Some(Ok(BodyFetchResponse {
        results: Vec::new(),
    }));
    press(f, KeyCode::Enter).await;
    press(f, KeyCode::End).await;
    keys(f, "o").await;
    assert!(f.screen.pending_open().is_some(), "waiting for the fetch");
    let failed = BodyFetchResponse {
        results: vec![BodyFetchResult {
            message_id: "m3".into(),
            outcome: BodyOutcome::Failed(OpError::internal("boom")),
        }],
    };
    let event = MailEvent::Bodies {
        thread_id: 1,
        result: Ok(failed),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
}

#[tokio::test]
async fn a_failed_fetch_ends_the_wait_so_a_later_landing_opens_nothing() {
    let mut f = fixture(false).await;
    open_then_fail_m3(&mut f).await;
    assert_eq!(f.screen.pending_open(), None);
    let html = Body {
        text: "late".into(),
        html: Some("<p>late</p>".into()),
        ..Body::default()
    };
    Store::open(&f.paths.db_file())
        .unwrap()
        .apply(
            &Batch {
                bodies: vec![("m3".into(), html)],
                ..Batch::new(1)
            },
            BASE_MS / 1000,
        )
        .unwrap();
    let landed = BodyFetchResponse {
        results: vec![BodyFetchResult {
            message_id: "m3".into(),
            outcome: BodyOutcome::Fetched,
        }],
    };
    let event = MailEvent::Bodies {
        thread_id: 1,
        result: Ok(landed),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(event)));
    settle(&mut f.screen, effects).await;
    let text = draw(&f, 90, 24);
    assert!(!text.contains("Open in the browser?"), "{text}");
    assert!(f.opened.lock().unwrap().is_empty());
}

#[tokio::test]
async fn o_after_a_failed_fetch_says_why_instead_of_waiting() {
    let mut f = fixture(false).await;
    open_then_fail_m3(&mut f).await;
    let calls = f.fake.calls.lock().unwrap().len();
    keys(&mut f, "o").await;
    let line = f.screen.status_line();
    assert!(line.contains("Cannot open this message"), "{line}");
    assert!(!line.contains("fetching"), "{line}");
    assert_eq!(f.screen.pending_open(), None);
    assert_eq!(f.fake.calls.lock().unwrap().len(), calls, "no new fetch");
}

#[tokio::test]
async fn an_html_answer_after_the_reader_closed_is_dropped() {
    let mut f = fixture(false).await;
    store_bodies_on_fetch(&f);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Home).await;
    keys(&mut f, "o").await;
    let confirm = f
        .screen
        .update(Msg::Key(crate::tui::test_util::key(KeyCode::Char('y'))));
    press(&mut f, KeyCode::Char('q')).await;
    settle(&mut f.screen, confirm).await;
    assert!(f.opened.lock().unwrap().is_empty());
    let dir = f.paths.cache_dir.join("open");
    assert!(!dir.exists() || std::fs::read_dir(&dir).unwrap().next().is_none());
}
