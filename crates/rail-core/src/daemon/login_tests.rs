//! Begun logins and consents through a real daemon: wiremock serves the token endpoint and the
//! test plays the browser by calling the loopback redirect itself.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;
use url::Url;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::client::DaemonClient;
use super::context::SaveGate;
use super::{Connector, ServeConfig};
use crate::auth::{FileSecretStore, SecretStore};
use crate::config::{AccountKind, load};
use crate::ops::op::{
    AccountAddBegin, AccountLoginBegin, ConsentBegin, DaemonStop, PendingCancel, PendingWait,
};
use crate::ops::{
    AccountAddRequest, CancelRequest, ClientSecretInput, ConsentBeginRequest, DaemonStopRequest,
    ErrorCode, LoginBeginRequest, OpError, PendingId, PendingKind, PendingOutcome, PendingState,
    Secret, WaitRequest, WaitResponse, names,
};
use crate::paths::Paths;
use crate::store::Store;
use crate::test_support::{daemon_context, start_daemon};

const ACCESS: &str = "the-access-token";
const REFRESH: &str = "the-refresh-token";
const CLIENT_SECRET: &str = "GOCSPX-test-secret";
const CODE: &str = "the-authorization-code";
const FILE_BACKEND: &str = "[general]\nsecrets = \"file\"\n";
const WORK: &str = "[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\nclient_secret = \"GOCSPX-test-secret\"\n";

struct Harness {
    _tmp: tempfile::TempDir,
    paths: Paths,
    client: DaemonClient,
    urls: UnboundedReceiver<Url>,
    gate: Arc<SaveGate>,
    begin_gate: Arc<super::context::BeginGate>,
    daemon: tokio::task::JoinHandle<Result<(), OpError>>,
    _token_server: MockServer,
}

async fn token_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/token"))
        .and(body_string_contains(format!("code={CODE}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "access_token": ACCESS, "refresh_token": REFRESH,
            "expires_in": 3600, "token_type": "Bearer"
        })))
        .mount(&server)
        .await;
    server
}

async fn harness(config: &str) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), config).unwrap();
    let server = token_server().await;
    let (ctx, urls) = daemon_context(&paths);
    let ctx = ctx.with_token_url(Some(format!("{}/token", server.uri())));
    let gate = ctx.save_gate.clone();
    let begin_gate = ctx.begin_gate.clone();
    let daemon = start_daemon(ctx, ServeConfig::default()).await;
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    Harness {
        _tmp: tmp,
        paths,
        client,
        urls,
        gate,
        begin_gate,
        daemon,
        _token_server: server,
    }
}

fn add_request(address: &str) -> AccountAddRequest {
    AccountAddRequest {
        kind: AccountKind::Gmail,
        address: address.to_string(),
        client_id: Some("cid".into()),
        client_secret: Some(ClientSecretInput::Value(Secret::new(CLIENT_SECRET))),
        tenant: None,
        name: None,
        color: None,
        store_secret_in_1password: false,
        open_browser: true,
    }
}

fn query(url: &Url, key: &str) -> String {
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| panic!("{key} missing from {url}"))
}

/// Play the browser: wait for the opener, then follow the redirect with `extra` in the query.
async fn browser(urls: &mut UnboundedReceiver<Url>, extra: &str) {
    let url = tokio::time::timeout(Duration::from_secs(5), urls.recv())
        .await
        .expect("the browser was asked")
        .unwrap();
    let redirect = query(&url, "redirect_uri");
    let state = query(&url, "state");
    reqwest::get(format!("{redirect}/?{extra}&state={state}"))
        .await
        .unwrap();
}

async fn wait(client: &DaemonClient, id: &PendingId, secs: u64) -> WaitResponse {
    let request = WaitRequest {
        id: id.clone(),
        timeout_secs: Some(secs),
    };
    client.call::<PendingWait>(&request).await.unwrap()
}

#[tokio::test]
async fn add_begin_returns_the_url_and_wait_reports_the_saved_account() {
    let mut h = harness(FILE_BACKEND).await;
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("Me@Example.com"))
        .await
        .unwrap();
    assert_eq!(begun.kind, PendingKind::AddAccount);
    assert!(begun.opened);
    assert!(begun.url.contains("code_challenge="), "{}", begun.url);
    browser(&mut h.urls, &format!("code={CODE}")).await;
    let finished = wait(&h.client, &begun.id, 10).await;
    let PendingState::Done {
        outcome: PendingOutcome::Added { account },
    } = finished.state
    else {
        panic!("{finished:?}");
    };
    assert_eq!(account.address, "me@example.com");
    assert_eq!(account.token_backend, "file");
    let secrets = FileSecretStore::new(h.paths.secrets_file());
    assert_eq!(
        secrets
            .get_tokens("me@example.com")
            .unwrap()
            .unwrap()
            .access_token,
        ACCESS
    );
    assert_eq!(
        Store::open(&h.paths.db_file())
            .unwrap()
            .list_accounts()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(load(&h.paths.config_file()).unwrap().accounts[0].name, "me");
}

#[tokio::test]
async fn a_refused_add_fails_at_begin_before_the_browser() {
    let mut h = harness(WORK).await;
    let err = h
        .client
        .call::<AccountAddBegin>(&add_request("ME@example.com"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Duplicate);
    assert!(
        h.urls.try_recv().is_err(),
        "no browser for a refused account"
    );
    // The daemon syncs the configured account (so the store exists), but the refused add
    // wrote nothing: no tokens, config unchanged.
    assert!(!h.paths.secrets_file().exists(), "no tokens written");
    assert_eq!(
        std::fs::read_to_string(h.paths.config_file()).unwrap(),
        WORK
    );
}

#[tokio::test]
async fn login_begin_replaces_tokens_and_leaves_config_alone() {
    let mut h = harness(WORK).await;
    let request = LoginBeginRequest {
        account: "WORK".into(),
        open_browser: true,
    };
    let begun = h.client.call::<AccountLoginBegin>(&request).await.unwrap();
    assert_eq!(begun.kind, PendingKind::Login);
    browser(&mut h.urls, &format!("code={CODE}")).await;
    let finished = wait(&h.client, &begun.id, 10).await;
    assert!(
        matches!(
            finished.state,
            PendingState::Done {
                outcome: PendingOutcome::LoggedIn { .. }
            }
        ),
        "{finished:?}"
    );
    let secrets = FileSecretStore::new(h.paths.secrets_file());
    assert_eq!(
        secrets
            .get_tokens("me@example.com")
            .unwrap()
            .unwrap()
            .refresh_token,
        REFRESH
    );
    assert_eq!(
        std::fs::read_to_string(h.paths.config_file()).unwrap(),
        WORK
    );
}

#[tokio::test]
async fn an_unknown_account_fails_login_begin_before_the_browser() {
    let mut h = harness(WORK).await;
    let request = LoginBeginRequest {
        account: "nobody".into(),
        open_browser: true,
    };
    let err = h
        .client
        .call::<AccountLoginBegin>(&request)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert!(h.urls.try_recv().is_err());
}

#[tokio::test]
async fn consent_begin_reports_the_tenant() {
    let mut h = harness(FILE_BACKEND).await;
    let request = ConsentBeginRequest {
        client_id: "cid".into(),
        tenant: "contoso.onmicrosoft.com".into(),
        open_browser: true,
    };
    let begun = h.client.call::<ConsentBegin>(&request).await.unwrap();
    assert!(begun.url.contains("/adminconsent"), "{}", begun.url);
    browser(&mut h.urls, "admin_consent=True&tenant=tid-9").await;
    let finished = wait(&h.client, &begun.id, 10).await;
    assert_eq!(
        finished.state,
        PendingState::Done {
            outcome: PendingOutcome::Consented {
                tenant: "tid-9".into()
            }
        }
    );
    let shared = ConsentBeginRequest {
        tenant: "common".into(),
        ..request
    };
    assert!(h.client.call::<ConsentBegin>(&shared).await.is_err());
}

#[tokio::test]
async fn a_provider_refusal_is_a_failed_state() {
    let mut h = harness(FILE_BACKEND).await;
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    browser(
        &mut h.urls,
        "error=access_denied&error_description=AADSTS65001",
    )
    .await;
    let finished = wait(&h.client, &begun.id, 10).await;
    let PendingState::Failed { error } = finished.state else {
        panic!("{finished:?}");
    };
    assert_eq!(error.code, ErrorCode::ConsentRequired);
    assert!(
        !h.paths.config_file().exists()
            || load(&h.paths.config_file()).unwrap().accounts.is_empty()
    );
}

#[tokio::test]
async fn cancel_before_the_redirect_saves_nothing() {
    let mut h = harness(FILE_BACKEND).await;
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    let url = h.urls.recv().await.unwrap();
    let cancel = CancelRequest {
        id: begun.id.clone(),
    };
    let cancelled = h.client.call::<PendingCancel>(&cancel).await.unwrap();
    assert_eq!(cancelled.state, PendingState::Cancelled);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let redirect = query(&url, "redirect_uri");
    let late = reqwest::get(format!(
        "{redirect}/?code={CODE}&state={}",
        query(&url, "state")
    ))
    .await;
    assert!(
        late.is_err(),
        "the loopback listener closed with the cancel"
    );
    assert_eq!(
        wait(&h.client, &begun.id, 1).await.state,
        PendingState::Cancelled
    );
    assert!(!h.paths.secrets_file().exists());
    assert!(!h.paths.db_file().exists());
    assert_eq!(
        std::fs::read_to_string(h.paths.config_file()).unwrap(),
        FILE_BACKEND
    );
}

/// Review focus 3.
#[tokio::test]
async fn a_client_disconnecting_during_the_save_does_not_stop_it() {
    let mut h = harness(FILE_BACKEND).await;
    h.gate.armed.store(true, Ordering::SeqCst);
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    browser(&mut h.urls, &format!("code={CODE}")).await;
    tokio::time::timeout(Duration::from_secs(5), h.gate.reached.notified())
        .await
        .expect("the save started");
    drop(h.client);
    tokio::time::sleep(Duration::from_millis(100)).await;
    h.gate.release.notify_one();
    let fresh = Connector::new(h.paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    let finished = wait(&fresh, &begun.id, 10).await;
    assert!(
        matches!(finished.state, PendingState::Done { .. }),
        "{finished:?}"
    );
    assert_eq!(load(&h.paths.config_file()).unwrap().accounts.len(), 1);
    let secrets = FileSecretStore::new(h.paths.secrets_file());
    assert!(secrets.get_tokens("me@example.com").unwrap().is_some());
}

#[tokio::test]
async fn cancel_during_the_save_is_refused_and_the_save_finishes() {
    let mut h = harness(FILE_BACKEND).await;
    h.gate.armed.store(true, Ordering::SeqCst);
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    browser(&mut h.urls, &format!("code={CODE}")).await;
    tokio::time::timeout(Duration::from_secs(5), h.gate.reached.notified())
        .await
        .expect("the save started");
    let cancel = CancelRequest {
        id: begun.id.clone(),
    };
    let answer = h.client.call::<PendingCancel>(&cancel).await.unwrap();
    assert_eq!(answer.state, PendingState::Saving);
    h.gate.release.notify_one();
    let finished = wait(&h.client, &begun.id, 10).await;
    assert!(
        matches!(finished.state, PendingState::Done { .. }),
        "{finished:?}"
    );
}

#[tokio::test]
async fn wait_times_out_while_the_user_is_still_in_the_browser() {
    let h = harness(FILE_BACKEND).await;
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    let still = wait(&h.client, &begun.id, 1).await;
    assert_eq!(still.state, PendingState::Waiting);
}

#[tokio::test]
async fn wait_and_cancel_report_an_unknown_id() {
    let h = harness(FILE_BACKEND).await;
    let id = PendingId("add-999".into());
    let request = WaitRequest {
        id: id.clone(),
        timeout_secs: Some(1),
    };
    let err = h.client.call::<PendingWait>(&request).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = h
        .client
        .call::<PendingCancel>(&CancelRequest { id })
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn begin_notifies_the_caller_when_the_login_finishes() {
    let mut h = harness(FILE_BACKEND).await;
    let mut notes = h.client.notifications();
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    browser(&mut h.urls, &format!("code={CODE}")).await;
    let note = tokio::time::timeout(Duration::from_secs(10), notes.recv())
        .await
        .expect("a notification")
        .unwrap();
    assert_eq!(note.method, names::PENDING_FINISHED);
    let finished: WaitResponse = serde_json::from_value(note.params).unwrap();
    assert_eq!(finished.id, begun.id);
    assert!(finished.state.is_finished());
}

#[tokio::test]
async fn a_pending_login_keeps_an_idle_daemon_alive() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), FILE_BACKEND).unwrap();
    let (ctx, _urls) = daemon_context(&paths);
    let config = ServeConfig {
        idle: Duration::from_millis(200),
        ..ServeConfig::default()
    };
    let daemon = start_daemon(ctx, config).await;
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    let begun = client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    drop(client);
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!daemon.is_finished(), "a login is still pending");
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    client
        .call::<PendingCancel>(&CancelRequest { id: begun.id })
        .await
        .unwrap();
    drop(client);
    let result = tokio::time::timeout(Duration::from_secs(5), daemon).await;
    result
        .expect("idle once nothing is pending")
        .unwrap()
        .unwrap();
}

/// Review focus 1, at the daemon: nothing it sends back carries a token, the client secret or
/// the authorization code.
#[tokio::test]
async fn no_daemon_response_carries_a_token_or_the_client_secret() {
    use crate::ops::op::{AccountsList, Status};
    use crate::ops::{AccountsListRequest, StatusRequest};
    let mut h = harness(FILE_BACKEND).await;
    let mut seen = Vec::new();
    let mut notes = h.client.notifications();
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    seen.push(serde_json::to_string(&begun).unwrap());
    browser(&mut h.urls, &format!("code={CODE}")).await;
    seen.push(serde_json::to_string(&wait(&h.client, &begun.id, 10).await).unwrap());
    let note = notes.recv().await.unwrap();
    seen.push(serde_json::to_string(&note).unwrap());
    let status = h.client.call::<Status>(&StatusRequest {}).await.unwrap();
    seen.push(serde_json::to_string(&status).unwrap());
    let listed = h
        .client
        .call::<AccountsList>(&AccountsListRequest {})
        .await
        .unwrap();
    seen.push(serde_json::to_string(&listed).unwrap());
    let duplicate = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap_err();
    seen.push(serde_json::to_string(&duplicate).unwrap());
    for text in &seen {
        for secret in [ACCESS, REFRESH, CLIENT_SECRET, CODE] {
            assert!(!text.contains(secret), "{secret} in {text}");
        }
    }
}

fn try_daemon_lock(paths: &Paths) -> bool {
    let file = std::fs::File::options()
        .read(true)
        .write(true)
        .open(paths.daemon_lock_file())
        .unwrap();
    fs4::FileExt::try_lock(&file).is_ok()
}

/// True once `daemon.lock` can be taken, within five seconds. A process another test forks
/// (an opener stand-in, a fake `op`) carries the daemon's descriptor until its `exec`, so the
/// lock may stay held for a moment after the daemon let go: one try is a race.
async fn lock_frees(paths: &Paths) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !try_daemon_lock(paths) {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    true
}

async fn stop(client: &DaemonClient) {
    client
        .call::<DaemonStop>(&DaemonStopRequest {})
        .await
        .unwrap();
}

/// Shutdown order: a save that is running finishes, and only then is `daemon.lock` released.
#[tokio::test]
async fn stop_waits_for_a_running_save_before_releasing_the_lock() {
    let mut h = harness(FILE_BACKEND).await;
    h.gate.armed.store(true, Ordering::SeqCst);
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    browser(&mut h.urls, &format!("code={CODE}")).await;
    tokio::time::timeout(Duration::from_secs(5), h.gate.reached.notified())
        .await
        .expect("the save started");
    stop(&h.client).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(!h.daemon.is_finished(), "the daemon waits for the save");
    assert!(!try_daemon_lock(&h.paths), "the lock is held until the end");
    h.gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), &mut h.daemon)
        .await
        .expect("the daemon stops once the save is done")
        .unwrap()
        .unwrap();
    assert!(lock_frees(&h.paths).await);
    assert!(!h.paths.socket_file().exists());
    assert_eq!(load(&h.paths.config_file()).unwrap().accounts.len(), 1);
    let _ = begun;
}

/// After shutdown a connection that was open before it is not served, and a login still
/// waiting for its browser is cancelled with its listener closed.
#[tokio::test]
async fn stop_ends_open_connections_and_waiting_logins() {
    let mut h = harness(FILE_BACKEND).await;
    let other = Connector::new(h.paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    let begun = h
        .client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    let url = h.urls.recv().await.unwrap();
    stop(&h.client).await;
    tokio::time::timeout(Duration::from_secs(10), &mut h.daemon)
        .await
        .expect("a waiting login does not hold shutdown")
        .unwrap()
        .unwrap();
    assert!(lock_frees(&h.paths).await);
    assert!(!h.paths.socket_file().exists());
    let status = other
        .call::<crate::ops::op::Status>(&crate::ops::StatusRequest {})
        .await;
    assert!(status.is_err(), "an old connection is not served");
    let redirect = query(&url, "redirect_uri");
    assert!(
        reqwest::get(format!("{redirect}/?code={CODE}"))
            .await
            .is_err()
    );
    let _ = begun;
}

/// At most `MAX_PENDING` logins wait at once; the next is refused before any browser opens.
#[tokio::test]
async fn too_many_waiting_logins_are_refused() {
    use crate::daemon::pending::MAX_PENDING;
    let mut h = harness(FILE_BACKEND).await;
    for n in 0..MAX_PENDING {
        let request = add_request(&format!("user{n}@example.com"));
        h.client.call::<AccountAddBegin>(&request).await.unwrap();
        h.urls.recv().await.unwrap();
    }
    let err = h
        .client
        .call::<AccountAddBegin>(&add_request("one-too-many@example.com"))
        .await
        .unwrap_err();
    assert_eq!(err.class, crate::error::ErrorClass::Transient);
    assert!(err.message.contains("too many logins"), "{err}");
    assert!(h.urls.try_recv().is_err(), "no browser for a refused begin");
}

/// A client that begins a login and then breaks the framing is disconnected at once; the
/// login, which belongs to the daemon, stays pending.
#[tokio::test]
async fn a_closed_connection_is_released_while_its_login_stays_pending() {
    use crate::daemon::wire::{MAX_FRAME_BYTES, Request, encode_frame, read_frame};
    use crate::daemon::{PROTOCOL_VERSION, VERSION};
    use tokio::io::{AsyncWriteExt, BufReader};
    let h = harness(FILE_BACKEND).await;
    let stream = tokio::net::UnixStream::connect(h.paths.socket_file())
        .await
        .unwrap();
    let (read, mut write) = stream.into_split();
    let hello = json!({"protocol": PROTOCOL_VERSION, "version": VERSION});
    let begin = serde_json::to_value(add_request("me@example.com")).unwrap();
    for request in [
        Request::new(1, names::HELLO, hello),
        Request::new(2, names::ACCOUNT_ADD_BEGIN, begin),
    ] {
        write
            .write_all(&encode_frame(&request).unwrap())
            .await
            .unwrap();
    }
    let mut reader = BufReader::new(read);
    for _ in 0..2 {
        read_frame(&mut reader).await.unwrap().unwrap();
    }
    let junk = vec![b'x'; MAX_FRAME_BYTES + 16];
    let _ = write.write_all(&junk).await;
    let closed = tokio::time::timeout(Duration::from_millis(500), async {
        while let Ok(Some(_)) = read_frame(&mut reader).await {}
    })
    .await;
    assert!(
        closed.is_ok(),
        "the socket must reach EOF while the login waits"
    );
    let begun = h
        .client
        .call::<crate::ops::op::DaemonInfoOp>(&Default::default())
        .await
        .unwrap();
    assert_eq!(begun.pending, 1);
}

/// A burst of begins held after their slot is reserved: exactly `MAX_PENDING` get through.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_of_begins_cannot_pass_the_cap() {
    use crate::daemon::pending::MAX_PENDING;
    let mut h = harness(FILE_BACKEND).await;
    h.begin_gate.armed.store(true, Ordering::SeqCst);
    let mut calls = tokio::task::JoinSet::new();
    for n in 0..MAX_PENDING + 4 {
        let client = h.client.clone();
        calls.spawn(async move {
            let request = add_request(&format!("burst{n}@example.com"));
            client.call::<AccountAddBegin>(&request).await
        });
    }
    let held = h.begin_gate.reached.acquire_many(MAX_PENDING as u32).await;
    held.unwrap().forget();
    h.begin_gate.open.send_replace(true);
    let (mut begun, mut refused) = (0, 0);
    while let Some(result) = calls.join_next().await {
        match result.unwrap() {
            Ok(_) => begun += 1,
            Err(e) => {
                assert!(e.message.contains("too many logins"), "{e}");
                refused += 1;
            }
        }
    }
    assert_eq!((begun, refused), (MAX_PENDING, 4));
    assert_eq!(
        h.begin_gate.reached.available_permits(),
        0,
        "no ninth got in"
    );
    for _ in 0..MAX_PENDING {
        h.urls.recv().await.unwrap();
    }
    assert!(h.urls.try_recv().is_err());
}

/// Once shutdown has started nothing new begins and no loopback listener is bound.
#[tokio::test]
async fn no_login_begins_once_shutdown_has_started() {
    let mut h = harness(FILE_BACKEND).await;
    h.gate.armed.store(true, Ordering::SeqCst);
    h.client
        .call::<AccountAddBegin>(&add_request("me@example.com"))
        .await
        .unwrap();
    browser(&mut h.urls, &format!("code={CODE}")).await;
    tokio::time::timeout(Duration::from_secs(5), h.gate.reached.notified())
        .await
        .expect("the save started");
    stop(&h.client).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let err = h
        .client
        .call::<AccountAddBegin>(&add_request("late@example.com"))
        .await
        .unwrap_err();
    assert!(err.message.contains("shutting down"), "{err}");
    assert!(h.urls.try_recv().is_err(), "nothing was bound or opened");
    h.gate.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), &mut h.daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

/// A begin that reserved a slot and bound its loopback before shutdown started is refused when
/// it reaches `begin`, and its loopback closes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_begin_in_flight_when_shutdown_starts_is_refused_and_its_loopback_closes() {
    let mut h = harness(FILE_BACKEND).await;
    h.begin_gate.armed.store(true, Ordering::SeqCst);
    let client = h.client.clone();
    let begin = tokio::spawn(async move {
        client
            .call::<AccountAddBegin>(&add_request("me@example.com"))
            .await
    });
    h.begin_gate.reached.acquire().await.unwrap().forget();
    let url = h.begin_gate.urls.lock().unwrap()[0].clone();
    let redirect = query(&url, "redirect_uri");
    stop(&h.client).await;
    h.begin_gate.open.send_replace(true);
    let err = begin.await.unwrap().unwrap_err();
    assert!(err.message.contains("shutting down"), "{err}");
    assert!(h.urls.try_recv().is_err(), "no browser for a refused begin");
    assert!(
        reqwest::get(format!("{redirect}/?code={CODE}"))
            .await
            .is_err(),
        "the loopback listener is closed"
    );
    tokio::time::timeout(Duration::from_secs(10), &mut h.daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
