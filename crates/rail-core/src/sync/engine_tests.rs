//! The engine with `MockProvider`, a real temporary store and the real writer.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{Notify, watch};

use super::engine::{Engine, EngineParts};
use super::status::StatusBoard;
use super::writer::{WriterHandle, WriterTask, spawn_writer};
use super::{Batch, Origin, UNREADABLE_BODY};
use crate::config::{AccountConfig, DAY_SECS, SyncSettings};
use crate::ops::{AccountSyncStatus, SyncPhase};
use crate::provider::mock::{Call, MockProvider, envelope};
use crate::provider::{ProviderError, Relabel, Window};
use crate::store::{Store, SyncStateRow};
use crate::test_support::sample_account;

const NOW: i64 = 20_000 * DAY_SECS;

/// `days` before `NOW`, in milliseconds, plus `extra` ms.
fn ago(days: i64, extra: i64) -> i64 {
    (NOW - days * DAY_SECS) * 1000 + extra
}

struct Harness {
    _tmp: tempfile::TempDir,
    db: PathBuf,
    writer: WriterHandle,
    _task: WriterTask,
    board: StatusBoard,
}

fn harness(accounts: &[&str]) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let db = tmp.path().join("rail.db");
    let store = Store::open(&db).unwrap();
    for address in accounts {
        store
            .conn()
            .execute(
                "INSERT INTO accounts (name, kind, address) VALUES (?1, 'gmail', ?1)",
                [address],
            )
            .unwrap();
    }
    let (writer, task) = spawn_writer(&db).unwrap();
    Harness {
        _tmp: tmp,
        db,
        writer,
        _task: task,
        board: StatusBoard::default(),
    }
}

fn settings(history_days: u32) -> SyncSettings {
    SyncSettings {
        history_days,
        body_cache_days: 30,
        poll_interval: Duration::from_secs(5),
    }
}

fn account(address: &str) -> AccountConfig {
    AccountConfig {
        name: address.split('@').next().unwrap().to_string(),
        address: address.to_string(),
        ..sample_account()
    }
}

fn engine_at(
    h: &Harness,
    mock: &MockProvider,
    address: &str,
    settings: SyncSettings,
    now: i64,
) -> Engine {
    let account = account(address);
    let id = Store::open_reader(&h.db)
        .unwrap()
        .account_id(address)
        .unwrap()
        .unwrap();
    h.board
        .insert(AccountSyncStatus::new(&account.name, address, account.kind));
    Engine::new(EngineParts {
        account,
        account_id: id,
        settings,
        provider: Arc::new(mock.clone()),
        writer: h.writer.clone(),
        reader: Store::open_reader(&h.db).unwrap(),
        board: h.board.clone(),
        wake: Arc::new(Notify::new()),
        outbox: Arc::new(Notify::new()),
        clock: Arc::new(move || now),
    })
}

fn engine(h: &Harness, mock: &MockProvider, history_days: u32) -> Engine {
    engine_at(h, mock, "a@example.com", settings(history_days), NOW)
}

fn stored(h: &Harness) -> Vec<String> {
    let store = Store::open_reader(&h.db).unwrap();
    let mut stmt = store
        .conn()
        .prepare("SELECT remote_id FROM messages ORDER BY remote_id")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn state(h: &Harness) -> SyncStateRow {
    Store::open_reader(&h.db).unwrap().sync_state(1).unwrap()
}

fn seed_days(mock: &MockProvider, days: &[(&str, i64)]) {
    for (id, d) in days {
        mock.seed(envelope(id, &format!("t-{id}"), ago(*d, 0)));
    }
}

#[tokio::test]
async fn a_backfill_then_deltas_keep_the_store_current() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 2);
    seed_days(
        &mock,
        &[("m1", 5), ("m2", 4), ("m3", 3), ("m4", 2), ("m5", 1)],
    );
    let engine = engine(&h, &mock, 0);
    engine.pass().await.unwrap();
    assert_eq!(stored(&h), ["m1", "m2", "m3", "m4", "m5"]);
    let after = state(&h);
    assert_eq!(after.cursor.as_deref(), Some("1"));
    assert_eq!((after.backfill_before, after.backfill_cursor), (None, None));
    assert_eq!(after.backfill_window, Some(0));
    let calls = mock.calls();
    assert_eq!(calls.iter().filter(|c| **c == Call::Profile).count(), 1);
    assert_eq!(calls.iter().filter(|c| **c == Call::Backfill).count(), 3);
    mock.add(envelope("m6", "t-m6", ago(0, 0)));
    mock.delete("m1");
    mock.relabel(Relabel {
        remote_id: "m2".into(),
        read: Some(true),
        ..Relabel::default()
    });
    engine.pass().await.unwrap();
    assert_eq!(stored(&h), ["m2", "m3", "m4", "m5", "m6"]);
    let read: i64 = Store::open_reader(&h.db)
        .unwrap()
        .conn()
        .query_row(
            "SELECT is_read FROM messages WHERE remote_id = 'm2'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(read, 1);
    assert_eq!(state(&h).cursor.as_deref(), Some("4"));
    assert_eq!(
        mock.calls().iter().filter(|c| **c == Call::Profile).count(),
        1,
        "no second backfill"
    );
}

/// A crash mid-backfill leaves Gmail's page token with the last committed page; a
/// restarted engine goes on with it, exactly where the listing stood, with the first cursor.
#[tokio::test]
async fn a_crash_mid_backfill_resumes_from_the_page_token_with_the_first_cursor() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 2);
    seed_days(
        &mock,
        &[("m1", 5), ("m2", 4), ("m3", 3), ("m4", 2), ("m5", 1)],
    );
    mock.fail_after(
        Call::Backfill,
        2,
        ProviderError::transient("the network went away"),
    );
    assert!(engine(&h, &mock, 0).pass().await.is_err());
    assert_eq!(stored(&h), ["m2", "m3", "m4", "m5"]);
    let crashed = state(&h);
    assert_eq!(crashed.cursor, None);
    assert_eq!(crashed.backfill_cursor.as_deref(), Some("1"));
    assert_eq!(crashed.backfill_before, Some(ago(4, 0)));
    let resume = crashed.backfill_resume.expect("the token is stored");
    assert_eq!(resume.listing, Window::default());
    mock.add(envelope("late", "t-late", ago(0, 0)));
    let restarted = engine(&h, &mock, 0);
    restarted.pass().await.unwrap();
    assert_eq!(
        stored(&h),
        ["late", "m1", "m2", "m3", "m4", "m5"],
        "the first cursor catches late mail"
    );
    assert_eq!(
        mock.calls().iter().filter(|c| **c == Call::Profile).count(),
        1
    );
    let tokens = mock.with(|m| m.list_tokens.clone());
    assert_eq!(
        tokens[3],
        Some(resume.token.0),
        "resumed with the stored token"
    );
    assert_eq!(
        mock.backfill_windows()[3],
        Window::default(),
        "and its query"
    );
    assert!(mock.metadata_refetches().is_empty());
    assert_eq!(state(&h).backfill_resume, None, "cleared at the end");
}

/// Review focus 1.
#[tokio::test]
async fn resume_keeps_messages_that_share_the_oldest_second() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| m.page_size = 2);
    for (id, extra) in [
        ("m1", 0),
        ("m2", 100),
        ("m3", 200),
        ("m4", 300),
        ("m5", 5_000),
    ] {
        mock.seed(envelope(id, "t", ago(3, extra)));
    }
    mock.fail_after(
        Call::Backfill,
        2,
        ProviderError::transient("the network went away"),
    );
    let engine = engine(&h, &mock, 0);
    assert!(engine.pass().await.is_err());
    assert_eq!(state(&h).backfill_before, Some(ago(3, 100)));
    engine.pass().await.unwrap();
    assert_eq!(stored(&h), ["m1", "m2", "m3", "m4", "m5"]);
}

#[tokio::test]
async fn a_message_arriving_during_backfill_is_not_lost() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| {
        m.page_size = 2;
        m.arrive_after_page = Some((1, envelope("newcomer", "t-new", ago(0, 0))));
    });
    seed_days(&mock, &[("m1", 4), ("m2", 3), ("m3", 2), ("m4", 1)]);
    engine(&h, &mock, 0).pass().await.unwrap();
    assert!(
        stored(&h).contains(&"newcomer".to_string()),
        "{:?}",
        stored(&h)
    );
}

#[tokio::test]
async fn an_expired_cursor_resyncs_in_full_and_sweeps_mail_gmail_lost() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    seed_days(&mock, &[("m1", 2), ("m2", 1)]);
    engine(&h, &mock, 0).pass().await.unwrap();
    mock.with(|m| {
        m.messages.remove("m1");
        m.history_id = 2;
        m.expired_below = 2;
    });
    let later = engine_at(&h, &mock, "a@example.com", settings(0), NOW + 60);
    later.pass().await.unwrap();
    assert_eq!(stored(&h), ["m2"]);
    assert_eq!(state(&h).cursor.as_deref(), Some("2"));
}

#[tokio::test]
async fn needs_auth_pauses_one_account_while_another_syncs() {
    let h = harness(&["a@example.com", "b@example.com"]);
    let (locked, fine) = (MockProvider::new(), MockProvider::new());
    seed_days(&fine, &[("b1", 1)]);
    seed_days(&locked, &[("a1", 1)]);
    engine(&h, &locked, 0).pass().await.unwrap();
    locked.fail_next(
        Call::Folders,
        ProviderError::NeedsAuth {
            detail: Some("revoked".into()),
        },
    );
    let a = engine(&h, &locked, 0);
    let wake_a = a.wake_handle();
    let b = engine_at(&h, &fine, "b@example.com", settings(0), NOW);
    let (stop, stopped) = watch::channel(false);
    let runs = [
        tokio::spawn(a.run(stopped.clone())),
        tokio::spawn(b.run(stopped)),
    ];
    let board = h.board.clone();
    wait_for(|| {
        board
            .get("a@example.com")
            .is_some_and(|s| s.needs_auth && s.phase == SyncPhase::NeedsAuth)
            && board
                .get("b@example.com")
                .is_some_and(|s| s.passes_completed >= 1)
    })
    .await;
    assert!(state(&h).needs_auth);
    assert_eq!(
        stored(&h),
        ["a1", "b1"],
        "a's mail is kept while it waits for a login"
    );
    let before = board.get("a@example.com").unwrap().passes_completed;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(
        board.get("a@example.com").unwrap().passes_completed,
        before,
        "paused: no retries"
    );
    wake_a.notify_one();
    wait_for(|| {
        board
            .get("a@example.com")
            .is_some_and(|s| !s.needs_auth && s.passes_completed > before)
    })
    .await;
    assert!(!state(&h).needs_auth);
    stop.send(true).unwrap();
    for run in runs {
        tokio::time::timeout(Duration::from_secs(5), run)
            .await
            .unwrap()
            .unwrap();
    }
}

async fn wait_for(mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the condition held in time");
}

#[tokio::test]
async fn shrinking_prunes_and_growing_backfills_further() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    seed_days(&mock, &[("d1", 1), ("d5", 5), ("d20", 20)]);
    let mut starred = envelope("s5", "t-s5", ago(5, 0));
    starred.is_starred = true;
    mock.seed(starred);
    engine(&h, &mock, 10).pass().await.unwrap();
    assert_eq!(stored(&h), ["d1", "d5", "s5"]);
    assert_eq!(state(&h).backfill_window, Some(ago(10, 0)));
    engine(&h, &mock, 2).pass().await.unwrap();
    assert_eq!(
        stored(&h),
        ["d1", "s5"],
        "starred mail stays; d5 is outside the window"
    );
    assert_eq!(state(&h).backfill_window, Some(ago(2, 0)));
    engine(&h, &mock, 30).pass().await.unwrap();
    assert_eq!(stored(&h), ["d1", "d20", "d5", "s5"]);
    let grow = *mock.backfill_windows().last().unwrap();
    assert_eq!(
        grow,
        Window {
            after_ms: Some(ago(30, 0)),
            before_ms: Some(ago(2, 1000))
        }
    );
}

#[tokio::test]
async fn search_hits_go_after_fourteen_days_without_a_search() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    for (id, at) in [("stale", NOW - 15 * DAY_SECS), ("fresh", NOW - DAY_SECS)] {
        let hit = Batch {
            upserts: vec![envelope(id, &format!("t-{id}"), ago(400, 0))],
            origin: Origin::Search,
            at: Some(at),
            ..Batch::new(1)
        };
        h.writer.write(hit).await.unwrap();
    }
    engine(&h, &mock, 30).pass().await.unwrap();
    assert_eq!(stored(&h), ["fresh"]);
}

#[tokio::test]
async fn a_message_rail_cannot_read_is_skipped_and_never_blocks_the_account() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    seed_days(&mock, &[("good", 1), ("bad", 2)]);
    mock.with(|m| {
        m.broken.insert("bad".into());
    });
    engine(&h, &mock, 0).pass().await.unwrap();
    assert_eq!(stored(&h), ["good"]);
    assert!(state(&h).cursor.is_some());
}

#[tokio::test]
async fn a_failed_pass_is_recorded_and_the_next_one_clears_it() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    seed_days(&mock, &[("m1", 1)]);
    let engine = engine(&h, &mock, 0);
    engine.pass().await.unwrap();
    mock.fail_next(Call::Changes, ProviderError::transient("Gmail is busy"));
    let failed = engine.pass().await;
    assert!(
        !engine.record(failed).await,
        "a transient failure does not pause"
    );
    let status = h.board.get("a@example.com").unwrap();
    assert!(status.last_pass_failed);
    assert_eq!(status.last_error.as_ref().unwrap().code, "transient");
    assert_eq!(status.next_attempt_at, Some(NOW + 5));
    assert_eq!(state(&h).last_error_code.as_deref(), Some("transient"));
    let ok = engine.pass().await;
    engine.record(ok).await;
    let status = h.board.get("a@example.com").unwrap();
    assert!(!status.last_pass_failed && status.last_error.is_none());
    assert_eq!(status.last_success_at, Some(NOW));
    assert_eq!((status.messages, status.threads), (1, 1));
    assert_eq!(state(&h).last_error_code, None);
}

#[tokio::test]
async fn a_stop_ends_the_engine_at_its_next_await_point() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    for i in 0..20 {
        mock.seed(envelope(&format!("m{i}"), "t", ago(1, i)));
    }
    mock.with(|m| m.body_delay = Duration::from_secs(30));
    let (stop, stopped) = watch::channel(false);
    let run = tokio::spawn(engine(&h, &mock, 0).run(stopped));
    let board = h.board.clone();
    wait_for(|| {
        board
            .get("a@example.com")
            .is_some_and(|s| s.phase == SyncPhase::Bodies)
    })
    .await;
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), run)
        .await
        .expect("stopped promptly")
        .unwrap();
}

#[tokio::test]
async fn a_wake_starts_a_pass_before_the_poll_interval() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    let engine = engine(&h, &mock, 0);
    let wake = engine.wake_handle();
    let (stop, stopped) = watch::channel(false);
    let run = tokio::spawn(engine.run(stopped));
    let board = h.board.clone();
    wait_for(|| {
        board
            .get("a@example.com")
            .is_some_and(|s| s.passes_completed == 1)
    })
    .await;
    wake.notify_one();
    wait_for(|| {
        board
            .get("a@example.com")
            .is_some_and(|s| s.passes_completed == 2)
    })
    .await;
    stop.send(true).unwrap();
    run.await.unwrap();
}

#[test]
fn the_board_publishes_updates_and_drops_gone_subscribers() {
    let board = StatusBoard::default();
    board.insert(AccountSyncStatus::new(
        "w",
        "W@example.com",
        crate::config::AccountKind::Gmail,
    ));
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = seen.clone();
    board.subscribe(Box::new(move |s| {
        sink.lock().unwrap().push(s.phase);
        true
    }));
    board.subscribe(Box::new(|_| false));
    board.update("w@example.com", |s| s.phase = SyncPhase::Delta);
    board.update("w@example.com", |s| s.phase = SyncPhase::Idle);
    assert_eq!(*seen.lock().unwrap(), [SyncPhase::Delta, SyncPhase::Idle]);
    assert_eq!(board.all().len(), 1);
    board.remove("w@example.com");
    assert!(board.get("w@example.com").is_none());
}

#[tokio::test]
async fn bodies_come_four_at_a_time_for_recent_threads_only() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    for i in 0..12 {
        mock.seed(envelope(&format!("r{i:02}"), &format!("t{i}"), ago(1, i)));
    }
    mock.seed(envelope("old", "t-old", ago(60, 0)));
    mock.with(|m| m.body_delay = Duration::from_millis(30));
    // The newest message's body is the first fetched; make it unreadable.
    mock.fail_next(
        Call::Body,
        ProviderError::permanent("could not read the body"),
    );
    engine(&h, &mock, 0).pass().await.unwrap();
    let max = mock.with(|m| m.max_bodies_in_flight);
    assert!((2..=4).contains(&max), "at most four bodies at once: {max}");
    let body = |id: &str| -> Option<String> {
        Store::open_reader(&h.db)
            .unwrap()
            .conn()
            .query_row(
                "SELECT body_text FROM messages WHERE remote_id = ?1",
                [id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let unreadable = (0..12)
        .filter(|i| body(&format!("r{i:02}")).as_deref() == Some(UNREADABLE_BODY))
        .count();
    assert_eq!(unreadable, 1, "one unreadable body is stored as the marker");
    assert_eq!(
        body("r05").map(|b| b.starts_with("body of") || b == UNREADABLE_BODY),
        Some(true)
    );
    assert_eq!(body("old"), None, "outside body_cache_days");
}

#[tokio::test]
async fn a_delta_with_more_waiting_reads_on_within_one_pass() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    let engine = engine(&h, &mock, 0);
    engine.pass().await.unwrap();
    for i in 0..5 {
        mock.add(envelope(&format!("n{i}"), &format!("t-n{i}"), ago(0, i)));
    }
    mock.with(|m| m.change_page = 2);
    let before = mock.calls().iter().filter(|c| **c == Call::Changes).count();
    engine.pass().await.unwrap();
    let rounds = mock.calls().iter().filter(|c| **c == Call::Changes).count() - before;
    assert_eq!(rounds, 3, "2 + 2 + 1 changes");
    assert_eq!(stored(&h), ["n0", "n1", "n2", "n3", "n4"]);
    assert_eq!(state(&h).cursor.as_deref(), Some("6"));
}

#[tokio::test]
async fn a_delta_that_says_more_without_moving_its_cursor_stops() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| m.always_more = true);
    let engine = engine(&h, &mock, 0);
    tokio::time::timeout(Duration::from_secs(5), engine.pass())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        mock.calls().iter().filter(|c| **c == Call::Changes).count(),
        1
    );
}

/// "Retrying" (set by the HTTP layer's retry observer) gives way to the step's own
/// phase once the call returns.
#[tokio::test]
async fn the_retrying_phase_gives_way_to_the_step_once_its_call_returns() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    mock.with(|m| {
        m.page_size = 1;
        m.page_delay = Duration::from_millis(300);
    });
    seed_days(&mock, &[("m1", 3), ("m2", 2), ("m3", 1)]);
    let engine = engine(&h, &mock, 0);
    let pass = tokio::spawn(async move { engine.pass().await });
    let board = h.board.clone();
    let pages = || {
        mock.calls()
            .iter()
            .filter(|c| **c == Call::Backfill)
            .count()
    };
    wait_for(|| {
        pages() == 1
            && board
                .get("a@example.com")
                .is_some_and(|s| s.phase == SyncPhase::Backfill)
    })
    .await;
    board.update("a@example.com", |s| {
        s.phase = SyncPhase::Retrying;
        s.next_attempt_at = Some(NOW + 30);
    });
    wait_for(|| {
        pages() == 2
            && board
                .get("a@example.com")
                .is_some_and(|s| s.phase == SyncPhase::Backfill && s.next_attempt_at.is_none())
    })
    .await;
    tokio::time::timeout(Duration::from_secs(10), pass)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// A stopped writer is permanent: the pass ends at once and nothing retries the write.
#[tokio::test]
async fn a_dead_writer_ends_the_pass_without_retrying_the_write() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    seed_days(&mock, &[("m1", 1)]);
    let kill = Batch {
        panic_writer: true,
        ..Batch::new(1)
    };
    assert!(h.writer.write(kill).await.is_err());
    let engine = engine(&h, &mock, 0);
    let failed = tokio::time::timeout(Duration::from_secs(5), engine.pass())
        .await
        .unwrap();
    let error = failed.unwrap_err();
    assert_eq!(error.code(), "store");
    assert_eq!(
        mock.calls(),
        [Call::Folders],
        "the pass ended at its first write"
    );
    let paused = tokio::time::timeout(Duration::from_secs(5), engine.record(Err(error)))
        .await
        .unwrap();
    assert!(!paused);
    let status = h.board.get("a@example.com").unwrap();
    assert_eq!(status.last_error.unwrap().code, "store");
}

#[tokio::test]
async fn a_login_failure_while_fetching_bodies_ends_the_pass() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    seed_days(&mock, &[("m1", 1), ("m2", 2)]);
    mock.fail_next(Call::Body, ProviderError::NeedsAuth { detail: None });
    let engine = engine(&h, &mock, 0);
    let error = engine.pass().await.unwrap_err();
    assert!(error.needs_auth());
    assert!(engine.record(Err(error)).await, "paused for a login");
    assert!(state(&h).needs_auth);
    assert_eq!(
        h.board.get("a@example.com").unwrap().phase,
        SyncPhase::NeedsAuth
    );
}

#[path = "pass_tests.rs"]
mod pass_tests;

#[path = "backfill_tests.rs"]
mod backfill_tests;

#[tokio::test]
async fn a_standing_provider_error_fails_a_pass_that_went_through() {
    let h = harness(&["a@example.com"]);
    let mock = MockProvider::new();
    seed_days(&mock, &[("m1", 1)]);
    let engine = engine(&h, &mock, 0);
    let text = "a folder cannot be synced: its sync link is longer than 4 KiB";
    let standing = ProviderError::permanent(text).with_detail("Projects".into());
    mock.with(|m| m.standing_error = Some(standing));
    let pass = engine.pass().await;
    assert!(pass.is_ok(), "the pass itself went through");
    engine.record(pass).await;
    let status = h.board.get("a@example.com").unwrap();
    assert!(status.last_pass_failed);
    let error = status.last_error.unwrap();
    assert_eq!(
        (error.code.as_str(), error.message.as_str()),
        ("permanent", text)
    );
    let detail = error.detail.as_ref().map(|d| d.get().as_str());
    assert_eq!(
        detail,
        Some("Projects"),
        "the folder's name, as outside text"
    );
    assert_eq!(
        (status.messages, status.threads),
        (1, 1),
        "the mail still synced"
    );
}
