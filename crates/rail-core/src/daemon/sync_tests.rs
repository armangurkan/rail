//! Sync through a real daemon: engines per account, the operations, notifications, search,
//! the idle rule, account removal and the client's display.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::UnixStream;

use super::client::DaemonClient;
use super::{Connector, ServeConfig, VERSION};
use crate::config::DAY_SECS;
use crate::ops::op::{AccountLoginBegin, DaemonStop, PendingCancel, Search, SyncNow, SyncStatus};
use crate::ops::{
    AccountSyncStatus, BrowserEnv, CancelRequest, DaemonStopRequest, ErrorCode, HitSource,
    LoginBeginRequest, OpError, SearchRequest, SyncNowRequest, SyncPhase, SyncStatusRequest, names,
};
use crate::paths::Paths;
use crate::provider::mock::{Call, MockProvider, envelope};
use crate::store::Store;
use crate::test_support::{
    daemon_context, daemon_context_with_browsers, mock_factory, start_daemon,
};

const WORK: &str = "[general]\nsecrets = \"file\"\npoll_interval = \"5s\"\nhistory_days = 30\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";
const CORP: &str = "\n[[accounts]]\nname = \"corp\"\nkind = \"m365\"\naddress = \"me@corp.example\"\nclient_id = \"cid2\"\n";

fn now_ms(days_ago: i64) -> i64 {
    (crate::auth::unix_now() - days_ago * DAY_SECS) * 1000
}

fn paths_with(tmp: &tempfile::TempDir, config: &str) -> Paths {
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), config).unwrap();
    paths
}

async fn daemon(
    paths: &Paths,
    mock: &MockProvider,
    serve: ServeConfig,
) -> (
    DaemonClient,
    tokio::task::JoinHandle<Result<(), crate::ops::OpError>>,
) {
    let (ctx, _) = daemon_context(paths);
    let ctx = ctx.with_provider_factory(mock_factory(mock));
    let task = start_daemon(ctx, serve).await;
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    (client, task)
}

async fn status_of(client: &DaemonClient, who: &str) -> Result<AccountSyncStatus, OpError> {
    let request = SyncStatusRequest {
        account: Some(who.into()),
    };
    Ok(client
        .call::<SyncStatus>(&request)
        .await?
        .accounts
        .remove(0))
}

/// Poll `sync.status` until `done`. An account not on the board yet (NotFound: it appears at
/// the next config check) is polled again.
async fn until(
    client: &DaemonClient,
    who: &str,
    done: impl Fn(&AccountSyncStatus) -> bool,
) -> AccountSyncStatus {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            match status_of(client, who).await {
                Ok(status) if done(&status) => return status,
                Ok(_) => {}
                Err(error) if error.code == ErrorCode::NotFound => {}
                Err(error) => panic!("sync.status failed: {error}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the status was reached in time")
}

#[tokio::test]
async fn sync_status_lists_every_configured_account() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, &format!("{WORK}{CORP}"));
    let mock = MockProvider::new();
    mock.seed(envelope("m1", "t1", now_ms(1)));
    let (client, _daemon) = daemon(&paths, &mock, ServeConfig::default()).await;
    let work = until(&client, "work", |s| s.passes_completed >= 1).await;
    assert_eq!((work.messages, work.threads), (1, 1));
    assert!(work.last_success_at.is_some() && work.last_error.is_none());
    let all = client
        .call::<SyncStatus>(&SyncStatusRequest::default())
        .await
        .unwrap();
    let corp = all.accounts.iter().find(|s| s.name == "corp").unwrap();
    assert_eq!(corp.phase, SyncPhase::Unsupported);
    let unknown = SyncStatusRequest {
        account: Some("nobody".into()),
    };
    assert_eq!(
        client.call::<SyncStatus>(&unknown).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn sync_now_reaches_its_target_pass_and_reports_progress() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let mock = MockProvider::new();
    let (client, _daemon) = daemon(&paths, &mock, ServeConfig::default()).await;
    until(&client, "work", |s| {
        s.passes_completed >= 1 && !s.pass_running
    })
    .await;
    let mut progress = client.notifications();
    let request = SyncNowRequest {
        account: Some("ME@example.com".into()),
        watch: true,
    };
    let targets = client.call::<SyncNow>(&request).await.unwrap().targets;
    assert_eq!(targets.len(), 1);
    assert_eq!(targets[0].name, "work");
    let target = targets[0].pass;
    let mut phases = Vec::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let note = progress.recv().await.unwrap();
            assert_eq!(note.method, names::SYNC_PROGRESS);
            let status: AccountSyncStatus = serde_json::from_value(note.params).unwrap();
            phases.push(status.phase);
            if status.passes_completed >= target {
                break;
            }
        }
    })
    .await
    .expect("progress arrived for the woken pass");
    assert!(phases.contains(&SyncPhase::Labels), "{phases:?}");
    let unknown = SyncNowRequest {
        account: Some("nobody".into()),
        watch: false,
    };
    assert_eq!(
        client.call::<SyncNow>(&unknown).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn sync_now_for_a_microsoft_account_says_it_is_not_supported_yet() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, &format!("{WORK}{CORP}"));
    let (client, _daemon) = daemon(&paths, &MockProvider::new(), ServeConfig::default()).await;
    let request = SyncNowRequest {
        account: Some("corp".into()),
        watch: false,
    };
    let err = client.call::<SyncNow>(&request).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Unsupported);
    assert!(err.message.contains("Microsoft 365"), "{err}");
}

#[tokio::test]
async fn search_merges_local_and_server_hits_and_keeps_server_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, &WORK.replace("history_days = 30", "history_days = 2"));
    let mock = MockProvider::new();
    let mut local = envelope("local1", "t-local", now_ms(1));
    local.subject = "budget plan".into();
    mock.seed(local);
    let mut old = envelope("old9", "t-old", now_ms(400));
    old.subject = "budget 2024".into();
    mock.seed(old);
    mock.with(|m| m.search_hits = vec!["old9".into(), "local1".into()]);
    let (client, _daemon) = daemon(&paths, &mock, ServeConfig::default()).await;
    until(&client, "work", |s| s.passes_completed >= 1).await;
    let request = SearchRequest {
        query: "budget".into(),
        account: None,
        limit: 10,
    };
    let found = client.call::<Search>(&request).await.unwrap();
    assert!(found.failures.is_empty(), "{:?}", found.failures);
    let got: Vec<(&str, HitSource)> = found
        .hits
        .iter()
        .map(|h| (h.thread_id.as_str(), h.source))
        .collect();
    assert_eq!(
        got,
        [("t-local", HitSource::Local), ("t-old", HitSource::Server)]
    );
    assert_eq!(found.hits[1].subject.get(), "budget 2024");
    assert_eq!(found.hits[1].sender.get(), "Sender <sender@example.com>");
    let origin: String = Store::open_reader(&paths.db_file())
        .unwrap()
        .conn()
        .query_row(
            "SELECT origin FROM messages WHERE remote_id = 'old9'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(origin, "search");
    let again = client.call::<Search>(&request).await.unwrap();
    assert_eq!(again.hits.len(), 2);
    let fetched = mock
        .calls()
        .iter()
        .filter(|c| **c == Call::Envelopes)
        .count();
    assert_eq!(
        fetched, 1,
        "the second search asked for no metadata it already had"
    );
    let empty = SearchRequest {
        query: "   ".into(),
        account: None,
        limit: 10,
    };
    assert_eq!(
        client.call::<Search>(&empty).await.unwrap_err().code,
        ErrorCode::InvalidInput
    );
}

/// PF15: on equal dates the local hit comes first, and one account narrows the search.
#[tokio::test]
async fn search_for_one_account_keeps_local_hits_first_on_equal_dates() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, &WORK.replace("history_days = 30", "history_days = 2"));
    let mock = MockProvider::new();
    let at = now_ms(1);
    let mut local = envelope("a1", "t-a", at);
    local.subject = "invoice".into();
    mock.seed(local);
    let mut old = envelope("b2", "t-b", now_ms(300));
    old.subject = "invoice".into();
    old.internal_date = at;
    old.date = at / 1000;
    mock.with(|m| m.search_hits = vec!["b2".into()]);
    let (client, _daemon) = daemon(&paths, &mock, ServeConfig::default()).await;
    until(&client, "work", |s| s.passes_completed >= 1).await;
    // Only now does Gmail have it: the backfill never saw it, so it is a server hit.
    mock.seed(old);
    let request = SearchRequest {
        query: "invoice".into(),
        account: Some("work".into()),
        limit: 10,
    };
    let found = client.call::<Search>(&request).await.unwrap();
    let got: Vec<(&str, HitSource)> = found
        .hits
        .iter()
        .map(|h| (h.thread_id.as_str(), h.source))
        .collect();
    assert_eq!(got, [("t-a", HitSource::Local), ("t-b", HitSource::Server)]);
    let unknown = SearchRequest {
        account: Some("nobody".into()),
        ..request
    };
    assert_eq!(
        client.call::<Search>(&unknown).await.unwrap_err().code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn the_daemon_stays_up_while_accounts_are_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let quick = ServeConfig {
        idle: Duration::from_millis(200),
        ..ServeConfig::default()
    };
    let (client, daemon) = daemon(&paths, &MockProvider::new(), quick).await;
    drop(client);
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(
        !daemon.is_finished(),
        "an account is configured, so the daemon stays"
    );
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    let _ = client.call::<DaemonStop>(&DaemonStopRequest {}).await;
    tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// Review focus 4.
#[tokio::test]
async fn removing_an_account_stops_its_engine_and_keeps_its_mail() {
    const SEEDED: u64 = 20;
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let mock = MockProvider::new();
    mock.with(|m| {
        m.page_size = 1;
        m.page_delay = Duration::from_millis(300);
    });
    for i in 0..SEEDED {
        let i = i64::try_from(i).unwrap();
        mock.seed(envelope(&format!("m{i}"), &format!("t{i}"), now_ms(1) + i));
    }
    let (client, _daemon) = daemon(&paths, &mock, ServeConfig::default()).await;
    until(&client, "work", |s| s.backfill_fetched >= 1).await;
    std::fs::write(paths.config_file(), "[general]\nsecrets = \"file\"\n").unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        while !client
            .call::<SyncStatus>(&SyncStatusRequest::default())
            .await
            .unwrap()
            .accounts
            .is_empty()
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the engine stopped");
    let reader = Store::open_reader(&paths.db_file()).unwrap();
    let kept = reader.counts(1).unwrap().messages;
    assert!(kept >= 1, "Rail never deletes mail when an account goes");
    assert!(kept < SEEDED, "the engine stopped mid-backfill ({kept})");
    assert!(reader.sync_state(1).unwrap().backfill_before.is_some());
    std::fs::write(paths.config_file(), WORK).unwrap();
    let back = until(&client, "work", |s| s.passes_completed >= 1).await;
    assert_eq!(back.messages, SEEDED);
    let profiles = mock.calls().iter().filter(|c| **c == Call::Profile).count();
    assert_eq!(profiles, 1, "the backfill resumed with its first cursor");
}

#[tokio::test]
async fn a_begun_login_opens_the_browser_with_the_connections_display() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let (ctx, mut opened) = daemon_context_with_browsers(&paths);
    let ctx = ctx.with_provider_factory(mock_factory(&MockProvider::new()));
    let _daemon = start_daemon(ctx, ServeConfig::default()).await;
    let stream = UnixStream::connect(paths.socket_file()).await.unwrap();
    let browser = BrowserEnv {
        display: Some(":42".into()),
        wayland_display: None,
        browser: Some("evil\nprogram".into()),
    };
    let client = DaemonClient::handshake_with(stream, VERSION, browser)
        .await
        .unwrap();
    let request = LoginBeginRequest {
        account: "work".into(),
        open_browser: true,
    };
    let begun = client.call::<AccountLoginBegin>(&request).await.unwrap();
    let (_url, env) = tokio::time::timeout(Duration::from_secs(5), opened.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(env.display.as_deref(), Some(":42"));
    assert_eq!(
        env.browser, None,
        "a value with a control character is dropped"
    );
    client
        .call::<PendingCancel>(&CancelRequest { id: begun.id })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_stop_during_sync_ends_in_time_with_the_store_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let mock = MockProvider::new();
    for i in 0..10 {
        mock.seed(envelope(&format!("m{i}"), "t", now_ms(1) + i));
    }
    mock.with(|m| m.body_delay = Duration::from_secs(60));
    let (client, daemon) = daemon(&paths, &mock, ServeConfig::default()).await;
    until(&client, "work", |s| s.phase == SyncPhase::Bodies).await;
    let _ = client.call::<DaemonStop>(&DaemonStopRequest {}).await;
    tokio::time::timeout(Duration::from_secs(15), daemon)
        .await
        .expect("stopped in time")
        .unwrap()
        .unwrap();
    let reader = Store::open_reader(&paths.db_file()).unwrap();
    assert_eq!(reader.counts(1).unwrap().messages, 10);
}

/// Fix round 1: at the default idle timeout a daemon with no accounts exits (the 2 s config
/// check must not keep re-arming the idle check).
#[tokio::test(start_paused = true)]
async fn a_daemon_without_accounts_idles_out_at_the_default_timeout() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, "[general]\nsecrets = \"file\"\n");
    let (ctx, _) = daemon_context(&paths);
    let daemon = start_daemon(ctx, ServeConfig::default()).await;
    tokio::time::timeout(super::IDLE_TIMEOUT + Duration::from_secs(60), daemon)
        .await
        .expect("the daemon idled out")
        .unwrap()
        .unwrap();
}

/// Fix round 1: accounts the server search skipped are reported, so the CLI can say so.
#[tokio::test]
async fn search_reports_the_accounts_it_could_not_ask() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, &format!("{WORK}{CORP}"));
    let (client, _daemon) = daemon(&paths, &MockProvider::new(), ServeConfig::default()).await;
    until(&client, "work", |s| s.passes_completed >= 1).await;
    let request = SearchRequest {
        query: "budget".into(),
        account: None,
        limit: 10,
    };
    let found = client.call::<Search>(&request).await.unwrap();
    assert_eq!(found.failures.len(), 1, "{:?}", found.failures);
    assert_eq!(found.failures[0].account, "corp");
    assert_eq!(found.failures[0].error.code, ErrorCode::Unsupported);
}

/// Review minor 8: a search of every account shows only the accounts configured now, never
/// the mail a removed account left in the store.
#[tokio::test]
async fn searching_every_account_leaves_out_removed_accounts_mail() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    {
        let store = Store::open(&paths.db_file()).unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO accounts (name, kind, address) VALUES ('gone', 'gmail', 'gone@example.com')",
                [],
            )
            .unwrap();
        let gone = store.account_id("gone@example.com").unwrap().unwrap();
        let (writer, task) = crate::sync::spawn_writer(&paths.db_file()).unwrap();
        let mut left_behind = envelope("g1", "t-gone", now_ms(1));
        left_behind.subject = "budget from a removed account".into();
        writer
            .write(crate::sync::Batch {
                upserts: vec![left_behind],
                ..crate::sync::Batch::new(gone)
            })
            .await
            .unwrap();
        drop(writer);
        assert!(task.finish(Duration::from_secs(5)).await);
    }
    let mock = MockProvider::new();
    let mut current = envelope("w1", "t-work", now_ms(2));
    current.subject = "budget for work".into();
    mock.seed(current);
    let (client, _daemon) = daemon(&paths, &mock, ServeConfig::default()).await;
    until(&client, "work", |s| s.passes_completed >= 1).await;
    let request = SearchRequest {
        query: "budget".into(),
        account: None,
        limit: 10,
    };
    let found = client.call::<Search>(&request).await.unwrap();
    let threads: Vec<&str> = found.hits.iter().map(|h| h.thread_id.as_str()).collect();
    assert_eq!(threads, ["t-work"]);
}
