//! The worker and snoozes: its idle sleep ends at the next snooze due; a snooze due is put back
//! on the next step, its return sent before the unread; while the account needs a login the store
//! still puts it back and the calls wait; the status board tells who watches; a wake that fails
//! waits longer each time before it is tried again.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};

use rusqlite::params;

use super::tests::{ADDRESS, Rig, rig};
use crate::auth::unix_now;
use crate::ops::SnoozeWoke;
use crate::provider::Change;
use crate::store::Store;
use crate::store::snoozes::SnoozeState;

/// Thread `remote` snoozed until `until`, as the daemon leaves it once its archive was sent:
/// `message` out of the inbox, and an active snooze recording it.
fn snoozed(rig: &Rig, remote: &str, message: &str, until: i64) {
    let store = Store::open(&rig.db).unwrap();
    let conn = store.conn();
    conn.execute(
        "DELETE FROM message_folders
         WHERE message_id = (SELECT id FROM messages WHERE remote_id = ?1)
           AND folder_id IN (SELECT id FROM folders WHERE kind = 'inbox')",
        [message],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO snoozes (account_id, thread_remote, until, created_at) VALUES (1, ?1, ?2, ?3)",
        params![remote, until, unix_now()],
    )
    .unwrap();
    let snooze = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO snooze_messages (snooze_id, message_remote) VALUES (?1, ?2)",
        params![snooze, message],
    )
    .unwrap();
}

/// Where `change` is among the calls the mock was given, if it was.
fn sent_at(rig: &Rig, change: &Change) -> Option<usize> {
    rig.mock
        .with(|m| m.applied.iter().position(|(_, c)| c == change))
}

fn back(message: &str) -> Change {
    Change::Unsnooze {
        messages: vec![message.into()],
    }
}

fn state_of(rig: &Rig) -> SnoozeState {
    rig.store().snoozes(1).unwrap()[0].state
}

#[tokio::test]
async fn the_idle_sleep_ends_at_the_next_snooze_due() {
    let rig = rig().await;
    snoozed(&rig, "t1", "m1", unix_now() + 20);
    let wait = rig.worker().idle().await;
    assert!((19..=20).contains(&wait.as_secs()), "{wait:?}");
}

#[tokio::test]
async fn a_snooze_due_goes_back_on_the_next_step_its_return_before_the_unread() {
    let mut rig = rig().await;
    snoozed(&rig, "t1", "m1", unix_now() - 1);
    rig.start();
    rig.until("the unread sent", |r| {
        sent_at(r, &Change::Read { on: false }).is_some()
    })
    .await;
    let (returned, unread) = (
        sent_at(&rig, &back("m1")).unwrap(),
        sent_at(&rig, &Change::Read { on: false }).unwrap(),
    );
    assert!(returned < unread, "the return goes first");
    assert_eq!(state_of(&rig), SnoozeState::Woken);
}

#[tokio::test]
async fn while_the_account_needs_a_login_the_store_puts_it_back_and_the_calls_wait() {
    let mut rig = rig().await;
    rig.board.update(ADDRESS, |s| s.needs_auth = true);
    snoozed(&rig, "t1", "m1", unix_now() - 1);
    rig.start();
    rig.until("woken in the store", |r| state_of(r) == SnoozeState::Woken)
        .await;
    assert_eq!(
        sent_at(&rig, &back("m1")),
        None,
        "nothing sent before a login"
    );
    rig.board.update(ADDRESS, |s| s.needs_auth = false);
    rig.signal.notify_one();
    rig.until("sent after the login", |r| {
        sent_at(r, &back("m1")).is_some()
    })
    .await;
}

#[tokio::test]
async fn a_wake_is_told_to_the_status_board() {
    let mut rig = rig().await;
    let woke = Arc::new(Mutex::new(Vec::new()));
    let seen = woke.clone();
    rig.board.subscribe_wakes(Box::new(move |w| {
        seen.lock().unwrap().push(w.clone());
        true
    }));
    snoozed(&rig, "t1", "m1", unix_now() - 1);
    let t1: i64 = rig
        .store()
        .conn()
        .query_row("SELECT id FROM threads WHERE remote_id = 't1'", [], |r| {
            r.get(0)
        })
        .unwrap();
    rig.start();
    rig.until("told", |_| !woke.lock().unwrap().is_empty())
        .await;
    assert_eq!(
        woke.lock().unwrap()[0],
        SnoozeWoke {
            account: "work".into(),
            threads: vec![t1],
        }
    );
}

/// A worker for the rig's account whose clock reads `at`.
fn worker_at(rig: &Rig, at: Arc<AtomicI64>) -> super::Worker {
    super::Worker::new(super::WorkerParts {
        name: "work".into(),
        address: ADDRESS.into(),
        account_id: 1,
        provider: Arc::new(rig.mock.clone()),
        writer: rig.writer.clone(),
        reader: Store::open_reader(&rig.db).unwrap(),
        board: rig.board.clone(),
        signal: rig.signal.clone(),
        clock: Arc::new(move || at.load(Ordering::SeqCst)),
        from_name: None,
        sync_wake: Arc::new(tokio::sync::Notify::new()),
        draft_files_dir: rig.draft_files_dir(),
        kind: crate::config::AccountKind::Gmail,
    })
}

#[tokio::test]
async fn the_worker_wakes_a_snooze_by_its_own_clock() {
    let rig = rig().await;
    // A week ahead of the wall clock: only the worker's clock can make the snooze due.
    let later = unix_now() + 7 * 86_400;
    snoozed(&rig, "t1", "m1", later + 30);
    let at = Arc::new(AtomicI64::new(later));
    let worker = worker_at(&rig, at.clone());
    assert_eq!(worker.idle().await.as_secs(), 30);
    worker.step().await;
    assert_eq!(state_of(&rig), SnoozeState::Active, "not due yet");
    at.store(later + 31, Ordering::SeqCst);
    worker.step().await;
    assert_eq!(state_of(&rig), SnoozeState::Woken);
}

#[tokio::test]
async fn the_idle_sleep_ends_at_the_earlier_of_a_due_row_and_a_due_snooze() {
    let rig = rig().await;
    let later = unix_now() + 7 * 86_400;
    snoozed(&rig, "t1", "m1", later + 40);
    let row = crate::store::outbox::NewAction {
        account_id: 1,
        action: crate::ops::Action::Star { on: true },
        threads: vec![
            rig.store()
                .conn()
                .query_row("SELECT id FROM threads WHERE remote_id = 't2'", [], |r| {
                    r.get(0)
                })
                .unwrap(),
        ],
        not_before: later + 15,
        client_token: "row".into(),
    };
    rig.writer
        .outbox(crate::store::outbox::OutboxWrite::Enqueue(row))
        .await
        .unwrap();
    let at = Arc::new(AtomicI64::new(later));
    assert_eq!(worker_at(&rig, at).idle().await.as_secs(), 15, "the row");
    let rig = super::tests::rig().await;
    snoozed(&rig, "t1", "m1", later + 20);
    let at = Arc::new(AtomicI64::new(later));
    assert_eq!(worker_at(&rig, at).idle().await.as_secs(), 20, "the snooze");
}

/// The store refuses to mark a snooze woken, so every wake fails; `false` lifts it.
fn refuse_wakes(rig: &Rig, refuse: bool) {
    let store = Store::open(&rig.db).unwrap();
    let sql = if refuse {
        "CREATE TRIGGER refuse_wake BEFORE UPDATE OF woken_at ON snoozes
         BEGIN SELECT RAISE(ABORT, 'refused'); END"
    } else {
        "DROP TRIGGER refuse_wake"
    };
    store.conn().execute(sql, []).unwrap();
}

#[tokio::test]
async fn a_wake_that_fails_is_tried_again_after_a_growing_wait() {
    let rig = rig().await;
    let later = unix_now() + 7 * 86_400;
    snoozed(&rig, "t1", "m1", later - 1);
    refuse_wakes(&rig, true);
    let at = Arc::new(AtomicI64::new(later));
    let worker = worker_at(&rig, at.clone());
    worker.step().await;
    assert_eq!(worker.idle().await.as_secs(), 2, "not every second");
    at.store(later + 2, Ordering::SeqCst);
    worker.step().await;
    assert_eq!(worker.idle().await.as_secs(), 4, "doubled");
    refuse_wakes(&rig, false);
    at.store(later + 5, Ordering::SeqCst);
    worker.step().await;
    assert_eq!(state_of(&rig), SnoozeState::Active, "still waiting");
    at.store(later + 6, Ordering::SeqCst);
    worker.step().await;
    assert_eq!(state_of(&rig), SnoozeState::Woken);
}

#[test]
fn the_wait_after_a_failed_wake_doubles_up_to_the_longest_idle() {
    use super::{MAX_IDLE, MIN_IDLE, next_wake_delay};
    assert_eq!(next_wake_delay(None), MIN_IDLE * 2);
    assert_eq!(next_wake_delay(Some(MIN_IDLE * 2)), MIN_IDLE * 4);
    assert_eq!(next_wake_delay(Some(MAX_IDLE / 2 + MIN_IDLE)), MAX_IDLE);
    assert_eq!(next_wake_delay(Some(MAX_IDLE)), MAX_IDLE);
}

#[tokio::test]
async fn the_wait_starts_afresh_once_no_snooze_is_due() {
    let rig = rig().await;
    let later = unix_now() + 7 * 86_400;
    snoozed(&rig, "t1", "m1", later - 1);
    refuse_wakes(&rig, true);
    let at = Arc::new(AtomicI64::new(later));
    let worker = worker_at(&rig, at.clone());
    worker.step().await;
    // The snooze is cleared meanwhile: the next look finds nothing due.
    let store = Store::open(&rig.db).unwrap();
    store.conn().execute("DELETE FROM snoozes", []).unwrap();
    at.store(later + 2, Ordering::SeqCst);
    worker.step().await;
    snoozed(&rig, "t2", "m2", later + 2);
    at.store(later + 3, Ordering::SeqCst);
    worker.step().await;
    assert_eq!(worker.idle().await.as_secs(), 2, "not doubled from before");
}
