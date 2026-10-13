//! Answers that land after a newer action took the undo record (issue #14): `z` takes back
//! only the newest action, but an older action's answer still shows its error, puts its
//! selection back, honours a `z` pressed for it, and a send's or a push's answer still makes
//! its record (or shows its error) when another message was started meanwhile, while `y` and
//! `s` on that message wait for it.

use rail_core::error::ErrorClass;
use rail_core::ops::{ActionApplyResponse, ErrorCode, RowToken};

use rail_core::store::drafts::{DraftKind, DraftWrite, Recipient};

use super::compose::{calls_of, compose_fixture, compose_open, composing, editor_appends};
use super::*;
use crate::tui::data::DaemonEvent;
use crate::tui::modals::Target;
use rail_core::store::drafts_folder::DraftsRowKind;

/// Run effects held back from a key, the way the app would once the daemon answers.
async fn answer(f: &mut Fixture, held: Vec<Effect>) {
    for effect in held {
        let results = run_job(effect).await;
        let effects = feed(f, results);
        settle_capturing(&mut f.screen, effects, &f.last_theme, &f.editor).await;
    }
}

/// A key whose effects (the daemon calls) are held back, not run.
fn press_held(f: &mut Fixture, code: KeyCode) -> Vec<Effect> {
    f.screen.update(Msg::Key(key(code)))
}

fn undos(f: &Fixture) -> Vec<Vec<RowToken>> {
    calls_of(f, |c| matches!(c, FakeCall::Undo { .. }))
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::Undo { rows } => Some(rows),
            _ => None,
        })
        .collect()
}

fn draft_sends(f: &Fixture) -> Vec<String> {
    calls_of(f, |c| matches!(c, FakeCall::DraftSend { .. }))
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::DraftSend { client_token, .. } => Some(client_token),
            _ => None,
        })
        .collect()
}

fn draft_pushes(f: &Fixture) -> Vec<String> {
    calls_of(f, |c| matches!(c, FakeCall::DraftPush { .. }))
        .into_iter()
        .filter_map(|c| match c {
            FakeCall::DraftPush { client_token, .. } => Some(client_token),
            _ => None,
        })
        .collect()
}

/// The fake answers an archive with `archive` and a star with row 20.
fn apply_answers(f: &Fixture, archive: Result<Vec<i64>, OpError>) {
    *f.fake.on_apply.lock().unwrap() = Some(Box::new(move |call| {
        let rows = match call.action {
            rail_core::ops::Action::Archive {} => archive.clone()?,
            _ => vec![20],
        };
        Ok(ActionApplyResponse {
            rows,
            applied: call.threads.len(),
        })
    }));
}

fn refused(text: &str) -> OpError {
    OpError::new(ErrorCode::Provider, ErrorClass::Permanent, text)
}

/// The review of a reply to the newest thread, saved as draft 1.
async fn review(f: &mut Fixture) {
    editor_appends(f, "x\n");
    press(f, KeyCode::Enter).await;
    press(f, KeyCode::Char('r')).await;
    assert!(compose_open(f));
}

/// `y` on the review with its answer held, then `q`: the review is closed while the send is
/// still in flight.
async fn send_then_close(f: &mut Fixture) -> Vec<Effect> {
    review(f).await;
    let held = press_held(f, KeyCode::Char('y'));
    assert!(compose_open(f), "the review waits for the answer");
    press(f, KeyCode::Char('q')).await;
    assert!(!compose_open(f));
    held
}

/// `s` on the review with its answer held, then `q`.
async fn push_then_close(f: &mut Fixture) -> Vec<Effect> {
    review(f).await;
    let held = press_held(f, KeyCode::Char('s'));
    assert!(compose_open(f), "the review waits for the answer");
    press(f, KeyCode::Char('q')).await;
    assert!(!compose_open(f));
    held
}

/// `r` on the open thread: a second review, of a new draft.
async fn another_reply(f: &mut Fixture) {
    press(f, KeyCode::Char('r')).await;
    assert!(compose_open(f), "a new reply is being written");
}

// Thread actions.

#[tokio::test]
async fn an_older_actions_success_leaves_z_on_the_newer_one() {
    let mut f = fixture(3).await;
    apply_answers(&f, Ok(vec![10]));
    let first = press_held(&mut f, KeyCode::Char('e'));
    let second = press_held(&mut f, KeyCode::Char('s'));
    answer(&mut f, second).await;
    assert!(f.screen.status_line().contains("starred 1 · z undoes"));
    answer(&mut f, first).await;
    assert!(
        f.screen.status_line().contains("starred 1 · z undoes"),
        "the older answer does not take the notice: {}",
        f.screen.status_line()
    );
    press(&mut f, KeyCode::Char('z')).await;
    let rows: Vec<Vec<i64>> = undos(&f)
        .into_iter()
        .map(|r| r.iter().map(|t| t.row).collect())
        .collect();
    assert_eq!(rows, [vec![20]], "only the newest action is undone");
}

#[tokio::test]
async fn an_older_actions_failure_shows_its_error_and_puts_its_selection_back() {
    let mut f = fixture(3).await;
    apply_answers(&f, Err(refused("the daemon said no")));
    press(&mut f, KeyCode::Char(' ')).await;
    press(&mut f, KeyCode::Char(' ')).await;
    let first = press_held(&mut f, KeyCode::Char('e'));
    assert!(f.screen.actions.selection.is_empty(), "the action took it");
    // The cursor row, not a selection: the newer action takes the record.
    let second = press_held(&mut f, KeyCode::Char('s'));
    answer(&mut f, second).await;
    answer(&mut f, first).await;
    assert!(
        f.screen.status_line().contains("Could not archive"),
        "{}",
        f.screen.status_line()
    );
    let archives = calls_of(&f, |c| {
        matches!(
            c,
            FakeCall::Apply {
                action: rail_core::ops::Action::Archive {},
                ..
            }
        )
    });
    assert_eq!(
        f.screen.error_count(),
        archives.len(),
        "one per account call"
    );
    assert_eq!(f.screen.actions.selection.len(), 2, "put back");
    press(&mut f, KeyCode::Char('z')).await;
    let rows: Vec<Vec<i64>> = undos(&f)
        .into_iter()
        .map(|r| r.iter().map(|t| t.row).collect())
        .collect();
    assert_eq!(rows, [vec![20]], "the newer action is still undoable");
}

#[tokio::test]
async fn z_pressed_for_an_older_action_still_undoes_it_when_it_answers() {
    let mut f = fixture(3).await;
    apply_answers(&f, Ok(vec![10]));
    let first = press_held(&mut f, KeyCode::Char('e'));
    press(&mut f, KeyCode::Char('z')).await;
    assert!(f.screen.status_line().contains("undoing…"));
    // A newer action before the first one's answer.
    press(&mut f, KeyCode::Char('s')).await;
    assert!(f.screen.status_line().contains("starred 1 · z undoes"));
    answer(&mut f, first).await;
    let rows: Vec<Vec<i64>> = undos(&f)
        .into_iter()
        .map(|r| r.iter().map(|t| t.row).collect())
        .collect();
    assert_eq!(rows, [vec![10]], "the z pressed for it is kept");
    assert!(
        f.screen.status_line().contains("starred 1 · z undoes"),
        "its undo is done quietly: {}",
        f.screen.status_line()
    );
    press(&mut f, KeyCode::Char('z')).await;
    let rows: Vec<Vec<i64>> = undos(&f)
        .into_iter()
        .map(|r| r.iter().map(|t| t.row).collect())
        .collect();
    assert_eq!(
        rows,
        [vec![10], vec![20]],
        "the newer action is still undoable"
    );
}

// draft.send.

#[tokio::test]
async fn a_send_answered_after_a_thread_action_is_what_z_cancels() {
    let mut f = compose_fixture().await;
    let held = send_then_close(&mut f).await;
    press(&mut f, KeyCode::Char('e')).await;
    answer(&mut f, held).await;
    assert!(
        f.screen.status_line().contains("sent · z undoes"),
        "{}",
        f.screen.status_line()
    );
    press(&mut f, KeyCode::Char('z')).await;
    let token = draft_sends(&f).pop().unwrap();
    assert_eq!(undos(&f).pop().unwrap()[0].client_token, token);
}

#[tokio::test]
async fn a_send_answered_while_another_reply_is_written_still_makes_its_record() {
    let mut f = compose_fixture().await;
    let held = send_then_close(&mut f).await;
    another_reply(&mut f).await;
    let other = composing(&f).draft;
    answer(&mut f, held).await;
    assert!(compose_open(&f), "the new reply stays open");
    assert_eq!(composing(&f).draft, other);
    assert!(
        f.screen.status_line().contains("sent · z undoes"),
        "{}",
        f.screen.status_line()
    );
    press(&mut f, KeyCode::Char('z')).await;
    let token = draft_sends(&f).pop().unwrap();
    assert_eq!(undos(&f).pop().unwrap()[0].client_token, token);
    assert!(f.screen.status_line().contains("undone"));
    assert_eq!(composing(&f).draft, other, "the new reply keeps its place");
}

#[tokio::test]
async fn a_failed_send_answered_while_another_reply_is_written_shows_its_error() {
    let mut f = compose_fixture().await;
    *f.fake.send_answer.lock().unwrap() = Some(Err(refused("the account is gone")));
    let held = send_then_close(&mut f).await;
    another_reply(&mut f).await;
    answer(&mut f, held).await;
    assert!(
        f.screen.status_line().contains("Could not send"),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(f.screen.error_count(), 1);
    assert!(
        composing(&f).errors.is_empty(),
        "the error is not put on another message's review"
    );
}

#[tokio::test]
async fn a_new_message_started_while_a_send_is_in_flight_does_not_drop_its_answer() {
    let mut f = compose_fixture().await;
    let held = send_then_close(&mut f).await;
    press(&mut f, KeyCode::Esc).await; // the reader closes
    press(&mut f, KeyCode::Char('1')).await; // work's inbox: no account question
    press(&mut f, KeyCode::Char('c')).await;
    assert!(compose_open(&f), "a new message is being written");
    answer(&mut f, held).await;
    assert!(
        f.screen.status_line().contains("sent · z undoes"),
        "{}",
        f.screen.status_line()
    );
}

// draft.push.

#[tokio::test]
async fn a_push_answered_while_another_reply_is_written_still_makes_its_record() {
    let mut f = compose_fixture().await;
    let held = push_then_close(&mut f).await;
    another_reply(&mut f).await;
    answer(&mut f, held).await;
    assert!(compose_open(&f), "the new reply stays open");
    assert!(
        f.screen.status_line().contains("saved to Gmail · z undoes"),
        "{}",
        f.screen.status_line()
    );
    press(&mut f, KeyCode::Char('z')).await;
    let token = draft_pushes(&f).pop().unwrap();
    assert_eq!(undos(&f).pop().unwrap()[0].client_token, token);
    assert!(f.screen.status_line().contains("kept on this computer"));
}

#[tokio::test]
async fn a_failed_push_answered_while_another_reply_is_written_shows_its_error() {
    let mut f = compose_fixture().await;
    *f.fake.push_answer.lock().unwrap() = Some(Err(refused("Gmail said no")));
    let held = push_then_close(&mut f).await;
    another_reply(&mut f).await;
    answer(&mut f, held).await;
    assert!(
        f.screen.status_line().contains("Could not save to Gmail"),
        "{}",
        f.screen.status_line()
    );
    assert_eq!(f.screen.error_count(), 1);
    assert!(composing(&f).errors.is_empty());
}

#[tokio::test]
async fn a_push_answered_after_a_thread_action_is_what_z_takes_back() {
    let mut f = compose_fixture().await;
    let held = push_then_close(&mut f).await;
    press(&mut f, KeyCode::Char('e')).await;
    answer(&mut f, held).await;
    assert!(f.screen.status_line().contains("saved to Gmail · z undoes"));
    press(&mut f, KeyCode::Char('z')).await;
    let token = draft_pushes(&f).pop().unwrap();
    assert_eq!(undos(&f).pop().unwrap()[0].client_token, token);
}

#[tokio::test]
async fn y_on_another_review_waits_while_a_send_is_in_flight() {
    let mut f = compose_fixture().await;
    let held = send_then_close(&mut f).await;
    another_reply(&mut f).await;
    press(&mut f, KeyCode::Char('y')).await;
    assert!(
        f.screen
            .status_line()
            .contains("Wait for the last message to be queued."),
        "{}",
        f.screen.status_line()
    );
    press(&mut f, KeyCode::Char('s')).await;
    assert!(draft_pushes(&f).is_empty(), "s waits too");
    assert!(draft_sends(&f).is_empty(), "the held first send is all");
    answer(&mut f, held).await;
    assert_eq!(draft_sends(&f).len(), 1);
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(draft_sends(&f).len(), 2, "then the second one goes");
}

#[tokio::test]
async fn a_too_late_undo_for_an_older_action_still_says_so() {
    let mut f = fixture(3).await;
    apply_answers(&f, Ok(vec![10]));
    *f.fake.undo_answer.lock().unwrap() = Some(Ok(rail_core::ops::ActionUndoResponse {
        undone: vec![],
        too_late: vec![10],
    }));
    let first = press_held(&mut f, KeyCode::Char('e'));
    press(&mut f, KeyCode::Char('z')).await;
    assert!(f.screen.status_line().contains("undoing…"));
    press(&mut f, KeyCode::Char('s')).await;
    answer(&mut f, first).await;
    assert!(
        f.screen.status_line().contains("too late to undo"),
        "{}",
        f.screen.status_line()
    );
    assert!(
        f.screen.actions.replaced.is_empty(),
        "the record is done with"
    );
}

// Drafts and the link while a send waits.

/// A local draft of `work` written straight into the store; it must get `id`.
fn seed(f: &Fixture, id: i64, subject: &str, at: i64) {
    let write = DraftWrite {
        id: None,
        account_id: 1,
        kind: DraftKind::New,
        thread_id: None,
        remote_thread_id: None,
        in_reply_to: None,
        references: None,
        to: vec![Recipient {
            name: None,
            address: "lee@example.com".into(),
        }],
        cc: vec![],
        bcc: vec![],
        subject: subject.into(),
        body: format!("body of {subject}\n"),
        attachments: vec![],
        message_id: format!("<d{id}@example.com>"),
        source_remote_id: None,
    };
    let made = writer(f).draft_upsert(&write, at).unwrap();
    assert_eq!(made, Some(id), "drafts are numbered in order");
}

/// `:drafts`, then Enter on the row of local draft `draft`.
async fn open_from_drafts(f: &mut Fixture, draft: i64) {
    let effects = f.screen.run_palette(Target::Drafts);
    settle_capturing(&mut f.screen, effects, &f.last_theme, &f.editor).await;
    let index = f
        .screen
        .drafts
        .picked
        .iter()
        .position(|r| matches!(r.kind, DraftsRowKind::Local { draft: d, .. } if d == draft))
        .unwrap();
    for _ in 0..index {
        press(f, KeyCode::Down).await;
    }
    press(f, KeyCode::Enter).await;
}

#[tokio::test]
async fn the_draft_being_sent_does_not_open_but_another_one_does() {
    let mut f = compose_fixture().await;
    // The review's save answers draft 1, as the store holds it.
    seed(&f, 1, "Being sent", BASE_MS / 1000 + 60);
    seed(&f, 2, "Another", BASE_MS / 1000 + 120);
    let held = send_then_close(&mut f).await;
    open_from_drafts(&mut f, 1).await;
    assert!(!compose_open(&f), "not while it is being sent");
    assert!(
        f.screen
            .status_line()
            .contains("This draft is being sent; wait for it to be queued."),
        "{}",
        f.screen.status_line()
    );
    open_from_drafts(&mut f, 2).await;
    assert!(compose_open(&f));
    assert_eq!(composing(&f).draft, Some(2));
    answer(&mut f, held).await;
    assert!(f.screen.status_line().contains("sent · z undoes"));
}

#[tokio::test]
async fn a_lost_link_while_a_send_waits_says_so_and_lets_y_send_again() {
    let mut f = compose_fixture().await;
    let _never = send_then_close(&mut f).await;
    let lost = MailEvent::Daemon(DaemonEvent::Lost(OpError::new(
        ErrorCode::Daemon,
        ErrorClass::Transient,
        "the connection to the daemon closed",
    )));
    let effects = f.screen.update(Msg::Task(TaskResult::Mail(lost)));
    settle(&mut f.screen, effects).await;
    let errors = f
        .screen
        .status
        .errors()
        .map(|e| e.text.clone())
        .collect::<Vec<_>>();
    assert!(
        errors
            .iter()
            .any(|e| e
                == "Could not confirm the send: the connection to the daemon closed · check Sent and Drafts"),
        "{errors:?}"
    );
    another_reply(&mut f).await;
    press(&mut f, KeyCode::Char('y')).await;
    assert_eq!(draft_sends(&f).len(), 1, "y sends again");
}

#[tokio::test]
async fn the_review_being_sent_does_not_edit_or_attach_until_it_is_queued() {
    let mut f = compose_fixture().await;
    review(&mut f).await;
    let held = press_held(&mut f, KeyCode::Char('y'));
    let saves_before = calls_of(&f, |c| matches!(c, FakeCall::DraftSave { .. })).len();
    let effects = press_held(&mut f, KeyCode::Char('e'));
    assert!(effects.is_empty(), "no editor: {effects:?}");
    assert!(
        f.screen
            .status_line()
            .contains("Wait for the last message to be queued."),
        "{}",
        f.screen.status_line()
    );
    let file = f.paths.data_dir.join("note.txt");
    std::fs::write(&file, "n").unwrap();
    press(&mut f, KeyCode::Char('a')).await;
    for c in file.to_string_lossy().chars() {
        press(&mut f, KeyCode::Char(c)).await;
    }
    press(&mut f, KeyCode::Enter).await;
    assert!(composing(&f).attachments.is_empty(), "nothing attached");
    let saves = calls_of(&f, |c| matches!(c, FakeCall::DraftSave { .. })).len();
    assert_eq!(saves, saves_before, "nothing saved");
    answer(&mut f, held).await;
    assert!(!compose_open(&f), "the send closed the review");
    assert!(f.screen.status_line().contains("sent · z undoes"));
}
