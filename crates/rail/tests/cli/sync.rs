//! `rail sync` and `rail search` through the real binary, against fake Gmail.

use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use std::io::BufRead;
use std::time::Duration;

use super::{
    KillsChild, StopsDaemon, WORK_FILE_BACKEND, bounded, finish_within, rail_cmd, stdout_of,
    write_config,
};

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
    gmail_with(Duration::ZERO, search_hits()).await
}

/// Gmail's search answer: the old `m9` and the recent `m1`.
fn search_hits() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(
        json!({"messages": [{"id": "m9", "threadId": "t-m9"}, {"id": "m1", "threadId": "t-m1"}]}),
    )
}

/// Fake Gmail whose labels step answers after `labels_delay` and whose search answers
/// `search`.
async fn gmail_with(labels_delay: Duration, search: ResponseTemplate) -> MockServer {
    let server = MockServer::start().await;
    let ok = |body: Value| ResponseTemplate::new(200).set_body_json(body);
    Mock::given(path(format!("{API}/labels")))
        .respond_with(
            ok(json!({"labels": [{"id": "INBOX", "name": "INBOX", "type": "system"}]}))
                .set_delay(labels_delay),
        )
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
        .respond_with(search)
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

/// `rail` for `dir`, its daemon's Gmail being `server`.
fn rail_at(dir: &tempfile::TempDir, server: &MockServer) -> std::process::Command {
    let mut cmd = rail_cmd(dir);
    cmd.env("RAIL_TEST_GMAIL_URL", format!("{}{API}/", server.uri()));
    cmd
}

fn sync_cmd(dir: &tempfile::TempDir, server: &MockServer, args: &[&str]) -> std::process::Output {
    bounded(rail_at(dir, server).args(args))
}

fn stderr_of(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
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

/// T9-A: a sync that stops changing is left to the daemon after the stall interval (one
/// second here, ten minutes for real), exiting 0 when nothing failed.
#[tokio::test]
async fn rail_sync_leaves_a_stalled_sync_to_the_daemon() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = gmail_with(Duration::from_secs(4), search_hits()).await;
    let out = bounded(
        rail_at(&dir, &server)
            .env("RAIL_TEST_SYNC_STALL_SECS", "1")
            .arg("sync"),
    );
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(stdout_of(&out).contains("work: reading labels"), "{out:?}");
    assert!(
        stderr_of(&out).contains("still syncing in the daemon; `rail doctor` shows progress"),
        "{out:?}"
    );
}

/// The first line `rail sync` prints that contains `want`, read as it is printed; the rest of
/// its standard output is collected by the returned thread.
fn wait_for_line(
    child: &mut std::process::Child,
    want: &'static str,
) -> std::thread::JoinHandle<String> {
    let stdout = child.stdout.take().unwrap();
    let (seen, saw) = std::sync::mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut all = String::new();
        for line in std::io::BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line.contains(want) {
                let _ = seen.send(());
            }
            all += &line;
            all.push('\n');
        }
        all
    });
    saw.recv_timeout(Duration::from_secs(20))
        .unwrap_or_else(|_| panic!("`{want}` never printed"));
    reader
}

/// Ctrl-C ends the watch only: exit 130 and a message, while the daemon's sync goes on and
/// completes.
#[tokio::test]
async fn ctrl_c_stops_watching_and_the_sync_carries_on() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = gmail_with(Duration::from_secs(3), search_hits()).await;
    let mut running = KillsChild(Some(
        rail_at(&dir, &server)
            .arg("sync")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let reader = wait_for_line(running.0.as_mut().unwrap(), "work: reading labels");
    let pid = running.0.as_ref().unwrap().id().to_string();
    let interrupted = std::process::Command::new("kill")
        .args(["-INT", &pid])
        .status()
        .unwrap();
    assert!(interrupted.success());
    let out = finish_within(running.0.take().unwrap(), Duration::from_secs(10));
    let printed = reader.join().unwrap();
    assert_eq!(out.status.code(), Some(130), "{out:?} {printed}");
    assert!(
        stderr_of(&out).contains("stopped watching; the sync carries on in the daemon"),
        "{out:?}"
    );
    assert!(!printed.contains("up to date"), "{printed}");
    // `daemon status` never starts a daemon: this is the one the sync runs in.
    let status = rail_cmd(&dir).args(["daemon", "status"]).output().unwrap();
    assert!(status.status.success(), "{status:?}");
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    loop {
        let doctor = sync_cmd(&dir, &server, &["doctor"]);
        if stdout_of(&doctor).contains(", last success ") {
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{doctor:?}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Review minor 9: a daemon that stops during a watch is followed: `rail sync` connects to a
/// new one, asks it to sync again and waits for the passes it names.
#[tokio::test]
async fn a_watch_follows_the_daemon_through_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = gmail_with(Duration::from_secs(2), search_hits()).await;
    let mut running = KillsChild(Some(
        rail_at(&dir, &server)
            .arg("sync")
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let reader = wait_for_line(running.0.as_mut().unwrap(), "work: reading labels");
    let stopped = bounded(rail_at(&dir, &server).args(["daemon", "stop"]));
    assert!(stopped.status.success(), "{stopped:?}");
    let out = finish_within(running.0.take().unwrap(), Duration::from_secs(30));
    let printed = reader.join().unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?} {printed}");
    assert!(
        stderr_of(&out).contains("the daemon restarted; asking the new one to sync"),
        "{out:?}"
    );
    assert!(printed.contains("work: up to date (1 message"), "{printed}");
}

/// With no Gmail account there is nothing to sync: a message and exit 0.
#[test]
fn rail_sync_without_accounts_says_so_and_exits_0() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, "[general]\nsecrets = \"file\"\n");
    let out = bounded(rail_cmd(&dir).arg("sync"));
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert_eq!(
        stdout_of(&out).trim(),
        "No Gmail account to sync; add one with `rail account add gmail`."
    );
}

/// A Gmail search that fails: local hits on stdout, the failure on stderr; exit 1 only when
/// nothing was found.
#[tokio::test]
async fn a_failed_gmail_search_keeps_local_hits_and_reports_on_stderr() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    // `Retry-After: 0` keeps the retries Rail makes on a 5xx from slowing the test.
    let broken = ResponseTemplate::new(500)
        .insert_header("Retry-After", "0")
        .set_body_json(json!({"error": {"code": 500, "message": "backend\nerror"}}));
    let server = gmail_with(Duration::ZERO, broken).await;
    assert_eq!(sync_cmd(&dir, &server, &["sync"]).status.code(), Some(0));
    let out = sync_cmd(&dir, &server, &["search", "budget"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let lines: Vec<String> = stdout_of(&out).lines().map(String::from).collect();
    assert_eq!(lines.len(), 1, "{out:?}");
    assert!(lines[0].contains("Budget for Q3"), "{out:?}");
    let stderr = stderr_of(&out);
    assert!(stderr.contains("work: not searched on Gmail: "), "{stderr}");
    assert!(!stderr.contains('\u{1b}'), "{stderr}");
    let none = sync_cmd(&dir, &server, &["search", "nothing-matches-this"]);
    assert_eq!(none.status.code(), Some(1), "{none:?}");
    assert_eq!(stdout_of(&none), "", "stdout carries result lines only");
    assert!(stderr_of(&none).contains("No messages found."), "{none:?}");
    assert!(
        stderr_of(&none).contains("work: not searched on Gmail: "),
        "{none:?}"
    );
}

/// T9-B: a Microsoft 365 account gives one note on stderr, and alone is no failure.
#[tokio::test]
async fn a_microsoft_account_gives_one_search_note() {
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let corp = "\n[[accounts]]\nname = \"corp\"\nkind = \"m365\"\naddress = \"c@x.io\"\nclient_id = \"c\"\n";
    write_config(&dir, &format!("{WORK_FILE_BACKEND}{corp}"));
    store_tokens(&dir);
    let server = gmail().await;
    assert_eq!(sync_cmd(&dir, &server, &["sync"]).status.code(), Some(0));
    let out = sync_cmd(&dir, &server, &["search", "budget"]);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stderr = stderr_of(&out);
    assert_eq!(
        stderr.trim(),
        "Microsoft 365 accounts are searched locally only until Microsoft support arrives",
        "{out:?}"
    );
}
