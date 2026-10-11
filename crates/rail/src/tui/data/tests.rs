//! The store reader against a seeded store, and the daemon link against an in-process daemon.

use std::time::Duration;

use rail_core::ops::op::DaemonStop;
use rail_core::ops::{BodyOutcome, DaemonStopRequest};
use rail_core::paths::Paths;
use rail_core::store::filter::parse;
use rail_core::store::view::PageAt;
use tokio::sync::mpsc;

use super::*;
use crate::tui::app::{Effect, TaskResult, TaskSink};
use crate::tui::test_util::{daemon_ctx, inbox_threads, seed_store};

const WORK: &str = "[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"w@example.com\"\nclient_id = \"cid\"\nclient_secret = \"sec\"\n";

/// Run `effect`'s job and collect what it sends within `limit`; the job keeps running in the
/// background when it has not finished.
async fn events(effect: Effect, want: usize, limit: Duration) -> Vec<MailEvent> {
    let Effect::Spawn(job) = effect else {
        panic!("expected a job, got {effect:?}");
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(job(TaskSink::new(tx, 0)));
    let mut got = Vec::new();
    let _ = tokio::time::timeout(limit, async {
        while got.len() < want {
            match rx.recv().await {
                Some((_, TaskResult::Mail(event))) => got.push(event),
                Some(_) => {}
                None => break,
            }
        }
    })
    .await;
    got
}

fn request(seq: u64, at: PageAt) -> ListRequest {
    ListRequest {
        seq,
        scope: ScopeChoice::Unified,
        addresses: vec!["w@example.com".into()],
        names: vec!["work".into()],
        filter: parse(""),
        at,
        utc_offset: 0,
    }
}

#[tokio::test]
async fn a_list_query_reads_a_page_with_its_sequence_number() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let envelopes: Vec<_> = inbox_threads("w", 120)
        .into_iter()
        .map(|e| (0, e))
        .collect();
    seed_store(&paths, &[("work", "w@example.com")], &envelopes);
    let reader = StoreReader::new(paths.db_file());
    let got = events(
        reader.list(request(7, PageAt::First)),
        1,
        Duration::from_secs(5),
    )
    .await;
    let [
        MailEvent::Listed {
            seq: 7,
            result: Ok(outcome),
        },
    ] = got.as_slice()
    else {
        panic!("{got:?}");
    };
    assert_eq!(outcome.page.rows.len(), 100);
    assert!(outcome.page.older);
    assert_eq!(outcome.account_ids, [Some(1)]);
    assert_eq!(outcome.at, PageAt::First);
}

#[tokio::test]
async fn an_unknown_account_in_the_filter_matches_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let envelopes: Vec<_> = inbox_threads("w", 3).into_iter().map(|e| (0, e)).collect();
    seed_store(&paths, &[("work", "w@example.com")], &envelopes);
    let reader = StoreReader::new(paths.db_file());
    let mut asked = request(1, PageAt::First);
    asked.filter = parse("account:home");
    let got = events(reader.list(asked), 1, Duration::from_secs(5)).await;
    let [
        MailEvent::Listed {
            result: Ok(outcome),
            ..
        },
    ] = got.as_slice()
    else {
        panic!("{got:?}");
    };
    assert!(outcome.page.rows.is_empty());
    let mut asked = request(2, PageAt::First);
    asked.filter = parse("account:WORK");
    let got = events(reader.list(asked), 1, Duration::from_secs(5)).await;
    let [
        MailEvent::Listed {
            result: Ok(outcome),
            ..
        },
    ] = got.as_slice()
    else {
        panic!("{got:?}");
    };
    assert_eq!(
        outcome.page.rows.len(),
        3,
        "account names match ignoring case"
    );
}

#[tokio::test]
async fn a_missing_or_unmigrated_store_is_not_set_up_until_it_is() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let reader = StoreReader::new(paths.db_file());
    assert!(matches!(
        reader.read(|_| Ok(())).await,
        Err(ReadError::NotSetUp)
    ));
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::write(paths.db_file(), b"").unwrap();
    assert!(
        matches!(reader.read(|_| Ok(())).await, Err(ReadError::NotSetUp)),
        "user_version 0"
    );
    std::fs::remove_file(paths.db_file()).unwrap();
    seed_store(&paths, &[("work", "w@example.com")], &[]);
    reader.read(|_| Ok(())).await.expect("set up now");
}

#[tokio::test]
async fn a_store_newer_than_this_build_is_an_error_not_a_hint() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let store = seed_store(&paths, &[("work", "w@example.com")], &[]);
    store
        .conn()
        .pragma_update(None, "user_version", 99)
        .unwrap();
    drop(store);
    let reader = StoreReader::new(paths.db_file());
    let Err(ReadError::Failed(error)) = reader.read(|_| Ok(())).await else {
        panic!("a newer store must not read as not set up");
    };
    assert!(error.message.contains("newer than this build"), "{error}");
}

/// Review Focus 1: the mail screen opens while the store is still at version 4 (the daemon
/// of the new build has not opened it yet). The list reads "not set up", never an SQL error
/// about `from_text` or `messages_fts`; once the daemon's `Store::open` has migrated it, the
/// next read works and finds the sender by a plain word.
#[tokio::test]
async fn a_store_mid_upgrade_reads_as_not_set_up_then_reads_after_the_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    let old = rail_core::store::Store::open_at_version(&paths.db_file(), 4).unwrap();
    old.conn()
        .execute_batch(
            "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', 'w@example.com');
             INSERT INTO folders (account_id, remote_id, name, kind) VALUES (1, 'INBOX', 'Inbox', 'inbox');
             INSERT INTO threads (account_id, remote_id, subject, last_date, message_count)
               VALUES (1, 't1', 'Q4 roadmap', 1760000000000, 1);
             INSERT INTO messages (account_id, thread_id, remote_id, subject, from_json, internal_date)
               VALUES (1, 1, 'm1', 'Q4 roadmap', '[{\"name\":\"Dana Reyes\",\"address\":\"dana@x.example\"}]',
                       1760000000000);
             INSERT INTO message_folders (message_id, folder_id) VALUES (1, 1);",
        )
        .unwrap();
    drop(old);
    let reader = StoreReader::new(paths.db_file());
    let mut asked = request(1, PageAt::First);
    asked.filter = parse("dana");
    let got = events(reader.list(asked.clone()), 1, Duration::from_secs(5)).await;
    assert!(
        matches!(
            got.as_slice(),
            [MailEvent::Listed {
                seq: 1,
                result: Err(ReadError::NotSetUp)
            }]
        ),
        "{got:?}"
    );

    // The daemon's writer opens the store and migrates it.
    drop(rail_core::store::Store::open(&paths.db_file()).unwrap());

    asked.seq = 2;
    let got = events(reader.list(asked), 1, Duration::from_secs(5)).await;
    let [
        MailEvent::Listed {
            seq: 2,
            result: Ok(outcome),
        },
    ] = got.as_slice()
    else {
        panic!("{got:?}");
    };
    assert_eq!(outcome.page.rows.len(), 1, "Dana is found by a plain word");
}

#[tokio::test]
async fn sidebar_thread_and_html_queries_report_back() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let envelopes: Vec<_> = inbox_threads("w", 2).into_iter().map(|e| (0, e)).collect();
    seed_store(&paths, &[("work", "w@example.com")], &envelopes);
    let reader = StoreReader::new(paths.db_file());
    let got = events(
        reader.sidebar(3, vec!["w@example.com".into()]),
        1,
        Duration::from_secs(5),
    )
    .await;
    let [
        MailEvent::Sidebar {
            seq: 3,
            result: Ok(sidebar),
        },
    ] = got.as_slice()
    else {
        panic!("{got:?}");
    };
    assert_eq!(sidebar.counts.accounts[0].threads, 2);
    let got = events(reader.thread(4, 1), 1, Duration::from_secs(5)).await;
    let [
        MailEvent::Thread {
            seq: 4,
            thread_id: 1,
            result: Ok(Some(detail)),
        },
    ] = got.as_slice()
    else {
        panic!("{got:?}");
    };
    assert_eq!(detail.messages.len(), 1);
    let got = events(
        reader.html(detail.messages[0].id),
        1,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        matches!(
            got.as_slice(),
            [MailEvent::Html {
                result: Ok(Some(rail_core::store::view::HtmlBody::NotFetched)),
                ..
            }]
        ),
        "{got:?}"
    );
}

#[tokio::test]
async fn a_list_read_overtaken_by_a_newer_one_skips_its_query() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let envelopes: Vec<_> = inbox_threads("w", 3).into_iter().map(|e| (0, e)).collect();
    seed_store(&paths, &[("work", "w@example.com")], &envelopes);
    let reader = StoreReader::new(paths.db_file());
    let older = reader.list(request(1, PageAt::First));
    let newer = reader.list(request(2, PageAt::First));
    let got = events(older, 1, Duration::from_secs(5)).await;
    assert!(
        matches!(
            got.as_slice(),
            [MailEvent::Listed {
                seq: 1,
                result: Err(ReadError::Stale)
            }]
        ),
        "{got:?}"
    );
    assert_eq!(
        reader.queries(),
        0,
        "the stale read never reached the store"
    );
    let got = events(newer, 1, Duration::from_secs(5)).await;
    assert!(
        matches!(
            got.as_slice(),
            [MailEvent::Listed {
                seq: 2,
                result: Ok(_)
            }]
        ),
        "{got:?}"
    );
    assert_eq!(reader.queries(), 1);
}

#[tokio::test]
async fn timers_report_their_token_after_the_delay() {
    let got = events(
        timer(Duration::from_millis(10), 5, Timer::Requery),
        1,
        Duration::from_secs(5),
    )
    .await;
    assert!(
        matches!(
            got.as_slice(),
            [MailEvent::Timer {
                token: 5,
                kind: Timer::Requery
            }]
        ),
        "{got:?}"
    );
}

#[test]
fn mail_events_print_no_mail_content() {
    let event = MailEvent::Daemon(DaemonEvent::Lost(rail_core::ops::OpError::internal("gone")));
    assert_eq!(format!("{event:?}"), "Daemon(Lost)");
    let listed = MailEvent::Listed {
        seq: 3,
        result: Err(ReadError::NotSetUp),
    };
    assert_eq!(format!("{listed:?}"), "Listed { seq: 3, ok: false }");
    let html = MailEvent::Html {
        message_id: 7,
        result: Ok(Some(rail_core::store::view::HtmlBody::Html(
            "<p>secret body</p>".into(),
        ))),
    };
    assert_eq!(format!("{html:?}"), "Html { message_id: 7 }");
    let body = rail_core::store::view::HtmlBody::Html("<p>secret body</p>".into());
    assert_eq!(format!("{body:?}"), "HtmlBody::Html(18 bytes)");
}

/// The real link: it subscribes, reports the configured accounts, fetches through the daemon
/// (an id the store does not hold is missing, with no network), and reports the loss when the
/// daemon stops.
#[tokio::test]
async fn the_link_watches_fetches_and_notices_the_daemon_going() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let (ctx, _urls) = daemon_ctx(&paths, None).await;
    let link = DaemonLink::new(ctx.daemon.clone());
    let got = events(link.watch(), 1, Duration::from_secs(10)).await;
    let [MailEvent::Daemon(DaemonEvent::Connected(statuses))] = got.as_slice() else {
        panic!("{got:?}");
    };
    assert_eq!(statuses[0].name, "work");
    let got = events(
        link.fetch_bodies(9, "work".into(), vec!["nope".into()]),
        1,
        Duration::from_secs(10),
    )
    .await;
    let [
        MailEvent::Bodies {
            thread_id: 9,
            result: Ok(response),
        },
    ] = got.as_slice()
    else {
        panic!("{got:?}");
    };
    assert_eq!(response.results.len(), 1);
    assert_eq!(response.results[0].message_id, "nope");
    assert!(
        matches!(
            response.results[0].outcome,
            BodyOutcome::Missing | BodyOutcome::Failed(_)
        ),
        "{response:?}"
    );
    let Effect::Spawn(job) = link.watch() else {
        panic!("a job");
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    tokio::spawn(job(TaskSink::new(tx, 0)));
    let connected = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap();
    assert!(matches!(
        connected,
        Some((
            _,
            TaskResult::Mail(MailEvent::Daemon(DaemonEvent::Connected(_)))
        ))
    ));
    ctx.daemon
        .call::<DaemonStop>(&DaemonStopRequest {})
        .await
        .unwrap();
    let lost = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Some((_, TaskResult::Mail(MailEvent::Daemon(DaemonEvent::Lost(error))))) => {
                    return error;
                }
                Some(_) => {}
                None => panic!("the job ended without reporting the loss"),
            }
        }
    })
    .await
    .expect("the loss was reported");
    assert_eq!(lost.code, rail_core::ops::ErrorCode::Daemon);
}

#[tokio::test]
async fn an_unreachable_daemon_is_reported_as_lost() {
    let link = DaemonLink::new(crate::tui::test_util::test_ctx().daemon);
    let got = events(link.watch(), 1, Duration::from_secs(10)).await;
    assert!(
        matches!(got.as_slice(), [MailEvent::Daemon(DaemonEvent::Lost(_))]),
        "{got:?}"
    );
}

#[test]
fn a_folder_choice_is_its_account_and_id_not_its_name() {
    let folder = |account, folder_id, name: &str| ScopeChoice::Folder {
        account,
        folder_id,
        name: name.into(),
    };
    assert_eq!(folder(0, 7, "Projects"), folder(0, 7, "Clients"));
    assert_ne!(folder(0, 7, "Projects"), folder(1, 7, "Projects"));
    assert_ne!(folder(0, 7, "Projects"), folder(0, 8, "Projects"));
    assert_ne!(ScopeChoice::Account(0), folder(0, 7, "Projects"));
}

#[tokio::test]
async fn a_refresh_job_reports_its_end_after_its_answers_and_after_a_panic() {
    let finished = refresh_job(3, |sink| async move {
        send(
            &sink,
            MailEvent::Timer {
                token: 1,
                kind: Timer::Requery,
            },
        );
    });
    let got = events(finished, 2, Duration::from_secs(10)).await;
    assert!(
        matches!(
            got.as_slice(),
            [MailEvent::Timer { .. }, MailEvent::RefreshEnded { seq: 3 }]
        ),
        "{got:?}"
    );

    let Effect::Spawn(job) = refresh_job(4, |_sink| async { panic!("boom") }) else {
        panic!("expected a job");
    };
    let (tx, mut rx) = mpsc::unbounded_channel();
    let joined = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::spawn(job(TaskSink::new(tx, 0))),
    )
    .await
    .expect("the job ended in time");
    assert!(
        joined.is_err_and(|e| e.is_panic()),
        "the panic goes on to the supervisor"
    );
    assert!(
        matches!(
            rx.try_recv(),
            Ok((_, TaskResult::Mail(MailEvent::RefreshEnded { seq: 4 })))
        ),
        "the refresh ended anyway"
    );
}
