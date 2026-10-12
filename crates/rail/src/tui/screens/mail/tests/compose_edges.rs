//! The composer's edges: a reply whose message leaves the store, forwarded parts removed,
//! the attachment size estimate, compose files swept at start, the text a crash left in a
//! compose file, and a late thread read.

use std::sync::{Arc, Mutex};

use rail_core::error::ErrorClass;
use rail_core::ops::drafts::{AttachmentSummary, DraftAttachment};
use rail_core::ops::{DraftSaveResponse, ErrorCode};

use super::compose::{compose_fixture, composing, editor_appends, saves, type_text};
use super::*;
use crate::tui::screens::compose::RowKind;

/// Answers each save as the daemon would about the message a reply names: refused when it
/// is no longer in the store. `requests` records every request.
fn daemon_like_saves(f: &Fixture, requests: Arc<Mutex<Vec<rail_core::ops::DraftSaveRequest>>>) {
    let db = f.paths.db_file();
    *f.fake.on_save.lock().unwrap() = Some(Box::new(move |call| {
        requests.lock().unwrap().push(call.request.clone());
        if let Some(reply) = &call.request.reply_to {
            let store = Store::open(&db).unwrap();
            if store.reply_source(reply.message).unwrap().is_none() {
                return Err(OpError::not_found(
                    "reply_to: the message being answered is no longer in the store",
                ));
            }
        }
        Ok(DraftSaveResponse {
            draft: 1,
            message_id: "<x@example.com>".into(),
            attachments: vec![],
        })
    }));
}

/// Final fix 1: only the create names the message answered; a later save keeps the stored
/// threading, so it still saves once that message is gone.
#[tokio::test]
async fn a_reply_still_saves_after_its_message_leaves_the_store() {
    let mut f = compose_fixture().await;
    editor_appends(&f, "more\n");
    let requests = Arc::new(Mutex::new(Vec::new()));
    daemon_like_saves(&f, requests.clone());
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await;
    let first = requests.lock().unwrap()[0].clone();
    let source = first
        .reply_to
        .expect("the create names the message")
        .message;
    assert_eq!(first.draft, None);
    writer(&f)
        .conn()
        .execute("DELETE FROM messages WHERE id = ?1", [source])
        .unwrap();
    press(&mut f, KeyCode::Char('e')).await;
    let sent = requests.lock().unwrap().clone();
    assert_eq!(
        sent.len(),
        3,
        "create, after the editor, after the second edit"
    );
    assert!(
        sent[1..].iter().all(|r| r.reply_to.is_none()),
        "updates name no message"
    );
    assert!(
        composing(&f).errors.is_empty(),
        "{:?}",
        composing(&f).errors
    );
    assert!(!f.screen.composing.unsaved);
    assert_eq!(saves(&f).len(), 3);
}

/// A summary of the forwarded part `p1` of `message`.
fn part_summary(message: i64) -> AttachmentSummary {
    AttachmentSummary {
        name: "plan.pdf".into(),
        mime: "application/pdf".into(),
        size: 1234,
        path: None,
        message: Some(message),
        part: Some("p1".into()),
    }
}

/// Answers each save as the daemon does with parts: a forward's create takes `p1` of the
/// message it names; an update keeps the parts it lists.
fn daemon_like_parts(f: &Fixture, requests: Arc<Mutex<Vec<rail_core::ops::DraftSaveRequest>>>) {
    *f.fake.on_save.lock().unwrap() = Some(Box::new(move |call| {
        let request = &call.request;
        requests.lock().unwrap().push(request.clone());
        let attachments = match (request.draft, &request.reply_to) {
            (None, Some(reply)) => vec![part_summary(reply.message)],
            _ => request
                .attachments
                .iter()
                .filter_map(|a| match a {
                    DraftAttachment::Part { message, .. } => Some(part_summary(*message)),
                    DraftAttachment::File { .. } => None,
                })
                .collect(),
        };
        Ok(DraftSaveResponse {
            draft: 1,
            message_id: "<x@example.com>".into(),
            attachments,
        })
    }));
}

fn lists_a_part(request: &rail_core::ops::DraftSaveRequest) -> bool {
    request
        .attachments
        .iter()
        .any(|a| matches!(a, DraftAttachment::Part { part, .. } if part == "p1"))
}

/// Final fix 2: an update lists the forwarded parts kept, so `x` on one removes it.
#[tokio::test]
async fn x_removes_a_forwarded_part_and_later_saves_leave_it_out() {
    let mut f = compose_fixture().await;
    let store = writer(&f);
    let message: i64 = store
        .conn()
        .query_row("SELECT id FROM messages WHERE remote_id = 'wm2'", [], |r| {
            r.get(0)
        })
        .unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO attachments (message_id, remote_part_id, filename, mime_type, size)
             VALUES (?1, 'p1', 'plan.pdf', 'application/pdf', 1234)",
            [message],
        )
        .unwrap();
    editor_appends(&f, "");
    let requests = Arc::new(Mutex::new(Vec::new()));
    daemon_like_parts(&f, requests.clone());
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('f')).await;
    {
        let sent = requests.lock().unwrap();
        assert_eq!(sent.len(), 2);
        assert!(!lists_a_part(&sent[0]), "a create names no part");
        assert!(lists_a_part(&sent[1]), "an update lists the part it keeps");
    }
    assert_eq!(
        composing(&f).attachments.len(),
        1,
        "the part is on the review"
    );
    while f.screen.compose.as_ref().unwrap().cursor_row_kind() != RowKind::Attachment(0) {
        press(&mut f, KeyCode::Char('j')).await;
    }
    press(&mut f, KeyCode::Char('x')).await;
    let last = requests.lock().unwrap().last().cloned().unwrap();
    assert!(
        !lists_a_part(&last),
        "the removal's save leaves the part out"
    );
    assert!(composing(&f).attachments.is_empty());
}

/// Final fix 3: `a` refuses what the worker would, counting attachments as encoded.
#[tokio::test]
async fn a_refuses_a_file_over_the_limit_once_encoded_without_a_save() {
    let mut f = compose_fixture().await;
    editor_appends(&f, "");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("big.bin");
    std::fs::File::create(&file)
        .unwrap()
        .set_len(20 * 1024 * 1024)
        .unwrap();
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await;
    let before = saves(&f).len();
    press(&mut f, KeyCode::Char('a')).await;
    type_text(&mut f, &file.to_string_lossy()).await;
    press(&mut f, KeyCode::Enter).await;
    assert_eq!(saves(&f).len(), before, "refused before any save");
    assert!(composing(&f).attachments.is_empty());
    let error = &composing(&f).errors[0];
    assert!(
        error.contains("25 MiB") && error.contains("26.6 MiB"),
        "{error}"
    );
}

/// Final fix 5: a crash's compose files go at the next start once a week old; newer ones and
/// files that are not compose files stay.
#[tokio::test]
async fn start_sweeps_compose_files_older_than_a_week() {
    let mut f = fixture(1).await;
    let dir = f.paths.compose_dir();
    std::fs::create_dir_all(&dir).unwrap();
    let (stale, fresh, other) = (dir.join("1.eml"), dir.join("2.eml"), dir.join("notes.txt"));
    for path in [&stale, &fresh, &other] {
        std::fs::write(path, "To: a@b.c\n\nx\n").unwrap();
    }
    let eight_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(8 * 86_400);
    for path in [&stale, &other] {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(eight_days_ago)
            .unwrap();
    }
    let effects = f.screen.start();
    settle(&mut f.screen, effects).await;
    assert!(!stale.exists(), "a week-old compose file is swept");
    assert!(fresh.exists(), "a newer one stays");
    assert!(other.exists(), "only .eml files are swept");
}

/// Final fix 8: a thread read that lands while the composer is open leaves the focus on it,
/// so its keys never act on the thread underneath.
#[tokio::test]
async fn a_late_thread_read_leaves_the_focus_on_the_composer() {
    let mut f = compose_fixture().await;
    editor_appends(&f, "");
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await;
    assert_eq!(f.screen.focus, Focus::Compose);
    let open = f.screen.reader.as_ref().unwrap().thread_id();
    let other = thread_ids(&f).into_iter().find(|t| *t != open).unwrap();
    let detail = writer(&f).thread_detail(other).unwrap();
    let late = MailEvent::Thread {
        seq: f.screen.latest_thread,
        thread_id: other,
        result: Ok(detail),
    };
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(late)));
    settle(&mut f.screen, effects).await;
    assert_eq!(
        f.screen.focus,
        Focus::Compose,
        "the composer keeps the keys"
    );
    assert!(f.screen.compose.is_some());
}

/// The fake editor records the text it was opened on, leaves it alone and exits 0.
fn editor_records(f: &Fixture) -> Arc<Mutex<Vec<String>>> {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    f.editor.set(move |path| {
        log.lock()
            .unwrap()
            .push(std::fs::read_to_string(path).unwrap());
        Ok(0)
    });
    seen
}

/// Leave `text` as the compose file `name` the way a crashed session would.
fn leave_compose_file(f: &Fixture, name: &str, text: &str) {
    let dir = f.paths.compose_dir();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(name), text).unwrap();
}

const RECOVERED: &str = "opened the text from your last editor session";

#[tokio::test]
async fn a_leftover_compose_file_is_opened_not_overwritten() {
    let mut f = compose_fixture().await;
    let leftover = "To: a@b.c\nCc: \nBcc: \nSubject: typed before the crash\n\nProbe version 1.\n";
    leave_compose_file(&f, "1.eml", leftover);
    let seen = editor_records(&f);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await; // saved as draft 1, then the editor opens
    assert_eq!(
        *seen.lock().unwrap(),
        [leftover],
        "the editor got the leftover"
    );
    assert!(f.screen.status_line().contains(RECOVERED));
    assert_eq!(composing(&f).headers.subject, "typed before the crash");
    assert!(composing(&f).body.contains("Probe version 1."));
}

#[tokio::test]
async fn a_compose_file_matching_the_store_is_written_as_usual() {
    let mut f = compose_fixture().await;
    let seen = editor_records(&f);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await; // no file: written from the store
    let stored = seen.lock().unwrap()[0].clone();
    assert!(stored.contains("Subject: Re: "), "{stored}");
    assert!(!f.screen.status_line().contains(RECOVERED));
    // A new session finds the very text the store would give: no recovery.
    let mut g = compose_fixture().await;
    leave_compose_file(&g, "1.eml", &stored);
    let seen = editor_records(&g);
    press(&mut g, KeyCode::Enter).await;
    press(&mut g, KeyCode::Char('r')).await;
    assert_eq!(*seen.lock().unwrap(), [stored]);
    assert!(!g.screen.status_line().contains(RECOVERED));
}

#[tokio::test]
async fn a_leftover_new_message_file_is_not_reused() {
    let mut f = compose_fixture().await;
    leave_compose_file(&f, "new.eml", "To: x@y.z\n\nsomeone else's message\n");
    // The first save fails, so the editor opens on a message with no draft id: `new.eml`.
    *f.fake.on_save.lock().unwrap() = Some(Box::new(|_| {
        Err(OpError::new(
            ErrorCode::Daemon,
            ErrorClass::Transient,
            "the connection to the daemon closed",
        ))
    }));
    let seen = editor_records(&f);
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await;
    assert_eq!(composing(&f).draft, None);
    let text = seen.lock().unwrap()[0].clone();
    assert!(!text.contains("someone else's message"), "{text}");
    assert!(!f.screen.status_line().contains(RECOVERED));
}

#[tokio::test]
async fn a_reopen_after_a_refused_header_is_on_the_same_edited_file() {
    let mut f = compose_fixture().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    f.editor.set(move |path| {
        let mut text = std::fs::read_to_string(path).unwrap();
        log.lock().unwrap().push(text.clone());
        // The first round adds a header Rail refuses; the second takes it out again.
        if log.lock().unwrap().len() == 1 {
            text.insert_str(0, "X-Bogus: 1\n");
        } else {
            text = text.replacen("X-Bogus: 1\n", "", 1);
        }
        std::fs::write(path, text).unwrap();
        Ok(0)
    });
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await;
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2, "refused, then the editor again");
    assert_eq!(seen[1], format!("X-Bogus: 1\n{}", seen[0]));
    assert!(!f.screen.status_line().contains(RECOVERED));
}

#[tokio::test]
async fn a_compose_file_that_cannot_be_read_is_kept_and_not_called_a_recovery() {
    let mut f = compose_fixture().await;
    // A directory at the path: reading it fails with something other than not found.
    let at = f.paths.compose_dir().join("1.eml");
    std::fs::create_dir_all(&at).unwrap();
    let runs = Arc::new(Mutex::new(Vec::new()));
    let log = runs.clone();
    f.editor.set(move |path| {
        log.lock().unwrap().push(path.to_path_buf());
        Ok(0)
    });
    press(&mut f, KeyCode::Enter).await;
    press(&mut f, KeyCode::Char('r')).await;
    assert_eq!(
        *runs.lock().unwrap(),
        std::slice::from_ref(&at),
        "the editor still ran"
    );
    assert!(at.is_dir(), "not overwritten");
    assert!(!f.screen.status_line().contains(RECOVERED));
}
