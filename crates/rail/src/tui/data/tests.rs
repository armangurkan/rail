//! The store reader against a seeded store, and the daemon link against an in-process daemon.

use std::time::Duration;

use rail_core::ops::op::DaemonStop;
use rail_core::ops::{
    BodyOutcome, DaemonStopRequest, DraftAttachment, DraftSaveRequest, ReplyTo, RowToken,
};
use rail_core::paths::Paths;
use rail_core::store::drafts::DraftKind;
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

#[tokio::test]
async fn the_fake_records_actions_and_answers_in_order() {
    use rail_core::ops::Action;
    let fake = std::sync::Arc::new(FakeDaemon::default());
    let link = DaemonLink::fake(fake.clone());
    let call = ApplyCall {
        seq: 1,
        account: "work".into(),
        threads: vec![4],
        action: Action::Archive {},
        grace: true,
        client_token: "tok-1".into(),
    };
    let bound = Duration::from_secs(5);
    let applied = events(link.apply(call.clone()), 1, bound).await;
    assert!(matches!(&applied[..], [MailEvent::Applied { result: Ok(r), .. }] if r.rows == [1]));
    let token = || RowToken {
        row: 1,
        client_token: "tok-1".into(),
    };
    events(link.undo(3, vec![token()]), 1, bound).await;
    events(link.retry(token()), 1, bound).await;
    assert_eq!(
        *fake.calls.lock().unwrap(),
        [
            FakeCall::Apply {
                account: "work".into(),
                threads: vec![4],
                action: Action::Archive {},
                grace: true
            },
            FakeCall::Undo {
                rows: vec![token()]
            },
            FakeCall::Retry { row: token() }
        ]
    );
}

#[test]
fn an_action_failure_debug_names_the_row_and_nothing_of_the_error() {
    use rail_core::error::ErrorClass;
    use rail_core::ops::{Action, ActionFailed, ErrorCode};
    let mut error = OpError::new(ErrorCode::Provider, ErrorClass::Permanent, "refused");
    error.detail = Some("SECRET gmail text".into());
    let event = MailEvent::ActionFailed(Box::new(ActionFailed {
        account: "SECRET-account".into(),
        row: 7,
        threads: vec![1, 2],
        action: Action::Archive {},
        error,
        draft: None,
        client_token: "tok".into(),
    }));
    let text = format!("{event:?}");
    assert!(
        text.contains("row: 7") && !text.contains("SECRET"),
        "{text}"
    );
}

fn save_request(kind: DraftKind) -> DraftSaveRequest {
    DraftSaveRequest {
        account: "work".into(),
        draft: None,
        kind,
        reply_to: (kind != DraftKind::New).then_some(ReplyTo { message: 3 }),
        to: vec!["dana@example.com".into()],
        cc: vec![],
        bcc: vec![],
        subject: "SECRET subject".into(),
        body: "SECRET body".into(),
        attachments: vec![DraftAttachment::File {
            path: "/tmp/SECRET.pdf".into(),
        }],
        client_token: "tok-save".into(),
    }
}

#[tokio::test]
async fn the_fake_records_draft_calls_and_answers_them() {
    let fake = std::sync::Arc::new(FakeDaemon::default());
    let link = DaemonLink::fake(fake.clone());
    let bound = Duration::from_secs(5);
    let call = SaveCall {
        seq: 2,
        request: save_request(DraftKind::New),
    };
    let saved = events(link.draft_save(call.clone()), 1, bound).await;
    let [
        MailEvent::DraftSaved {
            seq: 2,
            result: Ok(first),
        },
    ] = saved.as_slice()
    else {
        panic!("{saved:?}");
    };
    assert_eq!(first.message_id, "<fake@example.com>");
    let again = events(link.draft_save(call), 1, bound).await;
    let [
        MailEvent::DraftSaved {
            result: Ok(second), ..
        },
    ] = again.as_slice()
    else {
        panic!("{again:?}");
    };
    assert_ne!(first.draft, second.draft, "a fresh draft each time");
    let sent = events(link.draft_send(4, first.draft, "tok-send".into()), 1, bound).await;
    assert!(
        matches!(sent.as_slice(), [MailEvent::DraftSent { seq: 4, client_token, result: Ok(_), .. }] if client_token == "tok-send"),
        "{sent:?}"
    );
    let gone = events(link.draft_discard(first.draft), 1, bound).await;
    assert!(matches!(
        gone.as_slice(),
        [MailEvent::DraftDiscarded { result: Ok(()), .. }]
    ));
    assert_eq!(
        fake.calls.lock().unwrap()[0],
        FakeCall::DraftSave {
            kind: DraftKind::New,
            to: vec!["dana@example.com".into()],
            subject: "SECRET subject".into(),
            attachments: 1,
        }
    );
    let calls = fake.calls.lock().unwrap();
    assert_eq!(
        calls[2..],
        [
            FakeCall::DraftSend {
                draft: first.draft,
                client_token: "tok-send".into()
            },
            FakeCall::DraftDiscard { draft: first.draft }
        ]
    );
}

#[tokio::test]
async fn a_save_hook_and_the_scripted_answers_replace_the_defaults() {
    let fake = std::sync::Arc::new(FakeDaemon::default());
    let link = DaemonLink::fake(fake.clone());
    let bound = Duration::from_secs(5);
    *fake.on_save.lock().unwrap() = Some(Box::new(|call| {
        Err(OpError::invalid_input(format!("hooked {}", call.seq)))
    }));
    *fake.send_answer.lock().unwrap() = Some(Err(OpError::internal("no send")));
    *fake.discard_answer.lock().unwrap() = Some(Err(OpError::internal("no discard")));
    let call = SaveCall {
        seq: 9,
        request: save_request(DraftKind::Reply),
    };
    let saved = events(link.draft_save(call), 1, bound).await;
    assert!(matches!(
        saved.as_slice(),
        [MailEvent::DraftSaved {
            seq: 9,
            result: Err(_)
        }]
    ));
    let sent = events(link.draft_send(1, 5, "t".into()), 1, bound).await;
    assert!(matches!(
        sent.as_slice(),
        [MailEvent::DraftSent { result: Err(_), .. }]
    ));
    let gone = events(link.draft_discard(5), 1, bound).await;
    assert!(matches!(
        gone.as_slice(),
        [MailEvent::DraftDiscarded {
            draft: 5,
            result: Err(_)
        }]
    ));
}

#[test]
fn a_save_call_and_the_draft_events_debug_without_content() {
    let call = SaveCall {
        seq: 3,
        request: save_request(DraftKind::Forward),
    };
    let shown = format!("{call:?}");
    assert!(
        !shown.contains("SECRET") && !shown.contains("dana"),
        "{shown}"
    );
    assert!(
        shown.contains("seq: 3") && shown.contains("Forward"),
        "{shown}"
    );
    let saved = MailEvent::DraftSaved {
        seq: 3,
        result: Err(OpError::internal("SECRET detail")),
    };
    let sent = MailEvent::DraftSent {
        seq: 4,
        draft: 8,
        client_token: "SECRET-token".into(),
        result: Err(OpError::internal("SECRET detail")),
    };
    let edited = MailEvent::EditorDone(Err("cannot run SECRET".into()));
    for event in [saved, sent, edited] {
        let shown = format!("{event:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
    }
}

#[tokio::test]
async fn the_reader_answers_reply_sources_suggestions_and_drafts() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let envelopes: Vec<_> = inbox_threads("w", 2).into_iter().map(|e| (0, e)).collect();
    let mut store = seed_store(&paths, &[("work", "w@example.com")], &envelopes);
    let (message, thread, account): (i64, i64, i64) = store
        .conn()
        .query_row(
            "SELECT m.id, m.thread_id, m.account_id FROM messages m ORDER BY m.id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    let _ = &mut store;
    let reader = StoreReader::new(paths.db_file());
    let bound = Duration::from_secs(5);
    for target in [ReplyTarget::Message(message), ReplyTarget::Thread(thread)] {
        let got = events(reader.reply_source(1, target), 1, bound).await;
        let [
            MailEvent::ReplySource {
                seq: 1,
                result: Ok(Some(source)),
            },
        ] = got.as_slice()
        else {
            panic!("{got:?}");
        };
        assert_eq!((source.message, source.account_id), (message, account));
    }
    let missing = events(
        reader.reply_source(2, ReplyTarget::Message(9_999)),
        1,
        bound,
    )
    .await;
    assert!(matches!(
        missing.as_slice(),
        [MailEvent::ReplySource {
            result: Ok(None),
            ..
        }]
    ));
    let empty = events(reader.reply_source(3, ReplyTarget::Thread(9_999)), 1, bound).await;
    assert!(matches!(
        empty.as_slice(),
        [MailEvent::ReplySource {
            result: Ok(None),
            ..
        }]
    ));
    let found = events(reader.suggest(4, String::new()), 1, bound).await;
    let [
        MailEvent::Suggestions {
            seq: 4,
            result: Ok(entries),
        },
    ] = found.as_slice()
    else {
        panic!("{found:?}");
    };
    assert!(
        !entries.is_empty(),
        "the seeded senders are in the address book"
    );
    let missing = events(reader.draft(6, 999), 1, bound).await;
    assert!(matches!(
        missing.as_slice(),
        [MailEvent::Draft {
            seq: 6,
            result: Ok(None)
        }]
    ));
}

#[tokio::test]
async fn the_fake_records_push_and_import_and_answers_with_the_token() {
    let fake = std::sync::Arc::new(FakeDaemon::default());
    let link = DaemonLink::fake(fake.clone());
    let bound = Duration::from_secs(5);
    let pushed = events(link.draft_push(3, 7, "tok-push".into()), 1, bound).await;
    assert!(
        matches!(pushed.as_slice(), [MailEvent::DraftPushed { seq: 3, draft: 7, client_token, result: Ok(_) }] if client_token == "tok-push"),
        "{pushed:?}"
    );
    let imported = events(
        link.draft_import(4, "work".into(), 42, "tok-import".into()),
        1,
        bound,
    )
    .await;
    assert!(
        matches!(
            imported.as_slice(),
            [MailEvent::DraftImported {
                seq: 4,
                message: 42,
                result: Ok(_)
            }]
        ),
        "{imported:?}"
    );
    assert_eq!(
        *fake.calls.lock().unwrap(),
        [
            FakeCall::DraftPush {
                draft: 7,
                client_token: "tok-push".into()
            },
            FakeCall::DraftImport {
                account: "work".into(),
                message: 42,
                client_token: "tok-import".into()
            }
        ]
    );
    *fake.push_answer.lock().unwrap() = Some(Err(OpError::internal("no push")));
    *fake.import_answer.lock().unwrap() = Some(Err(OpError::internal("no import")));
    let pushed = events(link.draft_push(1, 7, "t".into()), 1, bound).await;
    assert!(matches!(
        pushed.as_slice(),
        [MailEvent::DraftPushed { result: Err(_), .. }]
    ));
    let imported = events(link.draft_import(2, "work".into(), 1, "t".into()), 1, bound).await;
    assert!(matches!(
        imported.as_slice(),
        [MailEvent::DraftImported { result: Err(_), .. }]
    ));
}

#[tokio::test]
async fn the_daemon_link_sends_the_push_and_import_requests_the_ops_name() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let (ctx, _urls) = daemon_ctx(&paths, None).await;
    let link = DaemonLink::new(ctx.daemon.clone());
    let bound = Duration::from_secs(10);
    let pushed = events(link.draft_push(1, 999, "tok".into()), 1, bound).await;
    let [
        MailEvent::DraftPushed {
            client_token,
            result: Err(_),
            ..
        },
    ] = pushed.as_slice()
    else {
        panic!("{pushed:?}");
    };
    assert_eq!(
        client_token, "tok",
        "the daemon knows the op and answers it"
    );
    let imported = events(
        link.draft_import(2, "work".into(), 999, "tok".into()),
        1,
        bound,
    )
    .await;
    assert!(
        matches!(
            imported.as_slice(),
            [MailEvent::DraftImported {
                message: 999,
                result: Err(_),
                ..
            }]
        ),
        "{imported:?}"
    );
}

#[tokio::test]
async fn the_reader_answers_the_drafts_folder_of_the_accounts_asked_for() {
    use rail_core::store::drafts::{DraftWrite, Recipient};
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    let mut store = seed_store(&paths, &[("work", "w@example.com")], &[]);
    let account: i64 = store
        .conn()
        .query_row("SELECT id FROM accounts LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let write = DraftWrite {
        id: None,
        account_id: account,
        kind: DraftKind::New,
        thread_id: None,
        remote_thread_id: None,
        in_reply_to: None,
        references: None,
        to: vec![Recipient {
            name: None,
            address: "dana@example.com".into(),
        }],
        cc: vec![],
        bcc: vec![],
        subject: "hello".into(),
        body: String::new(),
        attachments: vec![],
        message_id: "<a@example.com>".into(),
    };
    store.draft_upsert(&write, 1_760_000_000).unwrap();
    let reader = StoreReader::new(paths.db_file());
    let bound = Duration::from_secs(5);
    let mine = events(reader.drafts_folder(8, vec![account]), 1, bound).await;
    assert!(
        matches!(mine.as_slice(), [MailEvent::DraftsFolder { seq: 8, result: Ok(rows) }] if rows.len() == 1),
        "{mine:?}"
    );
    let other = events(reader.drafts_folder(9, vec![account + 100]), 1, bound).await;
    assert!(
        matches!(other.as_slice(), [MailEvent::DraftsFolder { seq: 9, result: Ok(rows) }] if rows.is_empty()),
        "{other:?}"
    );
}

#[test]
fn the_push_import_and_folder_events_debug_without_content() {
    let pushed = MailEvent::DraftPushed {
        seq: 1,
        draft: 2,
        client_token: "SECRET-token".into(),
        result: Err(OpError::internal("SECRET detail")),
    };
    let imported = MailEvent::DraftImported {
        seq: 3,
        message: 4,
        result: Err(OpError::internal("SECRET detail")),
    };
    let folder = MailEvent::DraftsFolder {
        seq: 5,
        result: Err(ReadError::Failed(OpError::internal("SECRET detail"))),
    };
    for event in [pushed, imported, folder] {
        let shown = format!("{event:?}");
        assert!(!shown.contains("SECRET"), "{shown}");
    }
}
