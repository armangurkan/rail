//! The worker and snoozes: its idle sleep ends at the next snooze due; a snooze due is put back
//! on the next step, its return sent before the unread; while the account needs a login the store
//! still puts it back and the calls wait; the status board tells who watches.

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
