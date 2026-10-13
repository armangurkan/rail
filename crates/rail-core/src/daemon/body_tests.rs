//! `body.fetch` through a real daemon over the fake Gmail (`MockProvider`): cached, uncached,
//! unreadable and missing bodies, a paused account, Microsoft 365, and bad requests.

use std::sync::Arc;
use std::time::Duration;

use super::client::DaemonClient;
use super::{Connector, ServeConfig};
use crate::config::DAY_SECS;
use crate::ops::op::{BodyFetch, SyncStatus};
use crate::ops::{
    AccountSyncStatus, BodyFetchRequest, BodyOutcome, ErrorCode, OpError, SyncStatusRequest,
};
use crate::paths::Paths;
use crate::provider::ProviderError;
use crate::provider::mock::{Call, MockProvider, envelope};
use crate::store::Store;
use crate::sync::UNREADABLE_BODY;
use crate::test_support::{daemon_context, mock_factory, start_daemon};

/// Bodies are cached for one day; mail ten days old has none after the first pass.
const WORK: &str = "[general]\nsecrets = \"file\"\npoll_interval = \"5s\"\nhistory_days = 365\nbody_cache_days = 1\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";
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

async fn daemon(paths: &Paths, mock: &MockProvider) -> DaemonClient {
    let (ctx, _) = daemon_context(paths);
    let ctx = ctx.with_provider_factory(mock_factory(mock));
    start_daemon(ctx, ServeConfig::default()).await;
    Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap()
}

async fn until(client: &DaemonClient, done: impl Fn(&AccountSyncStatus) -> bool) {
    until_for(client, "work", done).await;
}

async fn until_for(
    client: &DaemonClient,
    account: &str,
    done: impl Fn(&AccountSyncStatus) -> bool,
) {
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            let request = SyncStatusRequest {
                account: Some(account.into()),
            };
            if let Ok(mut found) = client.call::<SyncStatus>(&request).await
                && done(&found.accounts.remove(0))
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the status was reached in time");
}

async fn fetch(
    client: &DaemonClient,
    account: &str,
    ids: &[&str],
) -> Result<Vec<(String, BodyOutcome)>, OpError> {
    let request = BodyFetchRequest {
        account: account.into(),
        message_ids: ids.iter().map(|s| (*s).to_string()).collect(),
    };
    let response = client.call::<BodyFetch>(&request).await?;
    Ok(response
        .results
        .into_iter()
        .map(|r| (r.message_id, r.outcome))
        .collect())
}

fn body_calls(mock: &MockProvider) -> usize {
    mock.calls().iter().filter(|c| **c == Call::Body).count()
}

fn body_text(paths: &Paths, remote: &str) -> Option<String> {
    Store::open_reader(&paths.db_file())
        .unwrap()
        .conn()
        .query_row(
            "SELECT body_text FROM messages WHERE remote_id = ?1",
            [remote],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn cached_uncached_unreadable_and_missing_bodies() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let mock = MockProvider::new();
    mock.seed(envelope("new", "t1", now_ms(0)));
    mock.seed(envelope("old", "t2", now_ms(10)));
    mock.seed(envelope("broken", "t3", now_ms(10)));
    mock.seed(envelope("gone", "t4", now_ms(10)));
    let client = daemon(&paths, &mock).await;
    until(&client, |s| s.passes_completed >= 1 && !s.pass_running).await;
    assert_eq!(
        body_text(&paths, "new").as_deref(),
        Some("body of new"),
        "cached by the pass"
    );
    assert_eq!(body_text(&paths, "old"), None, "older than the body cache");
    // Unreadable only now: a broken envelope would be skipped by the pass and never stored.
    mock.with(|m| m.broken.insert("broken".into()));

    mock.fail_next(Call::Body, ProviderError::NotFound);
    let before = body_calls(&mock);
    assert_eq!(
        fetch(&client, "work", &["gone"]).await.unwrap(),
        [("gone".to_string(), BodyOutcome::Missing)]
    );
    assert_eq!(body_calls(&mock), before + 1);
    assert_eq!(body_text(&paths, "gone"), None);

    let before = body_calls(&mock);
    let results = fetch(
        &client,
        "ME@example.com",
        &["new", "old", "broken", "nowhere", "old"],
    )
    .await
    .unwrap();
    assert_eq!(
        results,
        [
            ("new".to_string(), BodyOutcome::Fetched),
            ("old".to_string(), BodyOutcome::Fetched),
            ("broken".to_string(), BodyOutcome::Unreadable),
            ("nowhere".to_string(), BodyOutcome::Missing),
        ],
        "one result per id, in order, repeats once"
    );
    assert_eq!(
        body_calls(&mock),
        before + 2,
        "only old and broken were asked for"
    );
    assert_eq!(body_text(&paths, "old").as_deref(), Some("body of old"));
    assert_eq!(
        body_text(&paths, "broken").as_deref(),
        Some(UNREADABLE_BODY)
    );
    let request = BodyFetchRequest {
        account: "work".into(),
        message_ids: vec!["old".into()],
    };
    let wire = serde_json::to_string(&client.call::<BodyFetch>(&request).await.unwrap()).unwrap();
    assert!(
        !wire.contains("body of old"),
        "outcomes, never bodies: {wire}"
    );
}

#[tokio::test]
async fn a_paused_account_answers_its_login_error_for_every_id() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let mock = MockProvider::new();
    mock.seed(envelope("old", "t1", now_ms(10)));
    mock.fail_next(Call::Folders, ProviderError::NeedsAuth { detail: None });
    let client = daemon(&paths, &mock).await;
    until(&client, |s| s.needs_auth).await;
    let before = body_calls(&mock);
    let results = fetch(&client, "work", &["old", "other"]).await.unwrap();
    assert_eq!(results.len(), 2);
    for (_, outcome) in &results {
        let BodyOutcome::Failed(error) = outcome else {
            panic!("{outcome:?}");
        };
        assert_eq!(error.code, ErrorCode::Auth);
        assert!(
            error
                .hint
                .as_deref()
                .unwrap()
                .contains("rail account login work")
        );
    }
    assert_eq!(
        body_calls(&mock),
        before,
        "a paused account asks Gmail nothing"
    );
}

/// A Microsoft 365 account's bodies come from its own engine's provider, into its own rows.
#[tokio::test]
async fn microsoft_365_reaches_its_engine_and_an_unknown_account_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, &format!("{WORK}{CORP}"));
    let mock = MockProvider::new();
    mock.seed(envelope("old", "t1", now_ms(10)));
    let client = daemon(&paths, &mock).await;
    until_for(&client, "corp", |s| {
        s.passes_completed >= 1 && !s.pass_running
    })
    .await;
    assert_eq!(corp_body(&paths, "old"), None, "older than the body cache");
    let before = body_calls(&mock);
    let results = fetch(&client, "corp", &["old", "nowhere"]).await.unwrap();
    assert_eq!(
        results,
        [
            ("old".to_string(), BodyOutcome::Fetched),
            ("nowhere".to_string(), BodyOutcome::Missing),
        ]
    );
    assert_eq!(body_calls(&mock), before + 1, "the provider was asked once");
    assert_eq!(corp_body(&paths, "old").as_deref(), Some("body of old"));
    let error = fetch(&client, "nobody", &["a"]).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

/// The body the store holds for `remote` in the Microsoft 365 account.
fn corp_body(paths: &Paths, remote: &str) -> Option<String> {
    Store::open_reader(&paths.db_file())
        .unwrap()
        .conn()
        .query_row(
            "SELECT m.body_text FROM messages m JOIN accounts a ON a.id = m.account_id \
             WHERE a.address = 'me@corp.example' AND m.remote_id = ?1",
            [remote],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn bad_requests_are_refused_without_echo() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let client = daemon(&paths, &MockProvider::new()).await;
    let many: Vec<String> = (0..51).map(|n| format!("m{n}")).collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    let error = fetch(&client, "work", &many).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidInput);
    assert!(error.message.contains("at most 50"), "{error}");
    for bad in ["../etc/passwd", "a b", "", "\u{1b}[2J", &"x".repeat(257)] {
        let error = fetch(&client, "work", &[bad]).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidInput, "{bad:?}");
        if !bad.is_empty() {
            assert!(!error.to_string().contains(bad), "{error}");
        }
    }
    assert!(fetch(&client, "work", &[]).await.unwrap().is_empty());
}
