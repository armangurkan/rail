//! `rail sync` and `rail search` through the real binary, against fake Gmail.

use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::{StopsDaemon, WORK_FILE_BACKEND, bounded, rail_cmd, stdout_of, write_config};

const API: &str = "/gmail/v1/users/me";

/// Tokens that stay fresh, so no test refreshes.
fn store_tokens(dir: &tempfile::TempDir) {
    use rail_core::auth::{FileSecretStore, SecretStore, TokenSet};
    let tokens = TokenSet {
        access_token: "test-access".into(),
        refresh_token: "test-refresh".into(),
        expires_at: 4_102_444_800,
    };
    FileSecretStore::new(dir.path().join("data").join("secrets.json"))
        .set_tokens("me@example.com", &tokens)
        .unwrap();
}

fn message(id: &str, subject: &str, days_ago: i64) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    json!({
        "id": id, "threadId": format!("t-{id}"), "labelIds": ["INBOX"], "snippet": "",
        "historyId": "10", "internalDate": ((now - days_ago * 86_400) * 1000).to_string(),
        "sizeEstimate": 10,
        "payload": {"mimeType": "text/plain", "headers": [
            {"name": "Subject", "value": subject},
            {"name": "From", "value": "Dana <dana@example.com>"}
        ], "body": {"size": 5, "data": "aGVsbG8"}}
    })
}

/// Fake Gmail with one recent message `m1`; `search` hits an old `m9` too.
async fn gmail() -> MockServer {
    let server = MockServer::start().await;
    let ok = |body: Value| ResponseTemplate::new(200).set_body_json(body);
    Mock::given(path(format!("{API}/labels")))
        .respond_with(ok(
            json!({"labels": [{"id": "INBOX", "name": "INBOX", "type": "system"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(path(format!("{API}/profile")))
        .respond_with(ok(json!({"emailAddress": "me@example.com", "messagesTotal": 1, "threadsTotal": 1, "historyId": "10"})))
        .mount(&server)
        .await;
    Mock::given(path(format!("{API}/history")))
        .respond_with(ok(json!({"history": [], "historyId": "10"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages")))
        .and(query_param("includeSpamTrash", "false"))
        .respond_with(ok(
            json!({"messages": [{"id": "m9", "threadId": "t-m9"}, {"id": "m1", "threadId": "t-m1"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages")))
        .and(query_param("includeSpamTrash", "true"))
        .respond_with(ok(json!({"messages": [{"id": "m1", "threadId": "t-m1"}]})))
        .mount(&server)
        .await;
    Mock::given(path(format!("{API}/messages/m1")))
        .respond_with(ok(message("m1", "Budget for Q3", 1)))
        .mount(&server)
        .await;
    Mock::given(path(format!("{API}/messages/m9")))
        .respond_with(ok(message("m9", "Old budget \u{1b}[2J\u{202E}", 400)))
        .mount(&server)
        .await;
    server
}

fn sync_cmd(dir: &tempfile::TempDir, server: &MockServer, args: &[&str]) -> std::process::Output {
    bounded(
        rail_cmd(dir)
            .env("RAIL_TEST_GMAIL_URL", format!("{}{API}/", server.uri()))
            .args(args),
    )
}

#[tokio::test]
async fn rail_sync_fills_the_store_and_exits_0_when_caught_up() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = gmail().await;
    let out = sync_cmd(&dir, &server, &["sync"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = stdout_of(&out);
    assert!(text.contains("work: up to date (1 message"), "{text}");
    // PF19: only the last success; the phase may be mid-pass after an idle restart.
    let doctor = sync_cmd(&dir, &server, &["doctor"]);
    let doctor_text = stdout_of(&doctor);
    assert!(
        doctor_text.contains("sync     work: ") && doctor_text.contains(", last success "),
        "{doctor:?}"
    );
}

#[tokio::test]
async fn rail_sync_no_wait_returns_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = gmail().await;
    let out = sync_cmd(&dir, &server, &["sync", "work", "--no-wait"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(stdout_of(&out).trim(), "work: syncing in the background");
}

#[tokio::test]
async fn rail_sync_exits_2_when_the_account_needs_a_login() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    let server = gmail().await;
    let out = sync_cmd(&dir, &server, &["sync"]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(
        stdout_of(&out).contains("work: needs a login; run `rail account login work`"),
        "{out:?}"
    );
}

#[tokio::test]
async fn rail_sync_exits_1_when_gmail_refuses() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = MockServer::start().await;
    Mock::given(path(format!("{API}/labels")))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": {"code": 400, "message": "nope\nnope"}})),
        )
        .mount(&server)
        .await;
    let out = sync_cmd(&dir, &server, &["sync"]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let text = stdout_of(&out);
    assert!(
        text.contains("work: sync failed: Gmail refused the request (HTTP 400)"),
        "{text}"
    );
    assert!(
        text.contains("\"nope\\nnope\""),
        "Gmail's words are quoted and escaped: {text}"
    );
}

#[tokio::test]
async fn rail_search_finds_old_mail_through_gmail_and_escapes_it() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(
        &dir,
        &WORK_FILE_BACKEND.replace("[general]\n", "[general]\nhistory_days = 2\n"),
    );
    store_tokens(&dir);
    let server = gmail().await;
    assert_eq!(sync_cmd(&dir, &server, &["sync"]).status.code(), Some(0));
    let out = sync_cmd(&dir, &server, &["search", "budget"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let text = stdout_of(&out);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "one line per thread: {text}");
    assert!(lines[0].contains("Budget for Q3"), "{text}");
    assert!(lines[1].contains("Old budget"), "{text}");
    assert!(!text.contains('\u{1b}') && !text.contains('\u{202E}'));
    let none = sync_cmd(&dir, &server, &["search", "--account", "nobody", "budget"]);
    assert_eq!(none.status.code(), Some(1), "{none:?}");
}
