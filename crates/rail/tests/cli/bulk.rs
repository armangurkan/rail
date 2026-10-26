//! Bulk selection in the real binary, in a pseudo-terminal against a mocked Gmail (AC-31):
//! `* a` counts the whole mailbox in the header, then `e` archives every selected thread, so
//! the provider receives one `threads/{id}/modify` per thread; a Shift range archives exactly
//! the threads it covers; `z` within the grace leaves Gmail untouched. Linux only, like every
//! pty test.

use std::io::Write;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::pty::{LIMIT, collect, drawn_row, script_available, text, wait_until};
use super::{StopsDaemon, WORK_FILE_BACKEND, rail_env, write_config};

/// Fake Gmail's prefix, as `cli/sync.rs` mounts it (private to that module, so copied).
const API: &str = "/gmail/v1/users/me";
/// The messages fake Gmail serves: one per thread, so one archive call per thread.
pub(super) const MESSAGES: [&str; 3] = ["m1", "m2", "m3"];
/// How often the provider is polled for the archive calls.
const POLL: Duration = Duration::from_millis(50);
/// Past the 10 s undo grace and the outbox worker's next turn: an archive `z` took back would
/// have reached Gmail by then.
const PAST_THE_GRACE: Duration = Duration::from_secs(15);

/// Kills the child if the test fails half way. `pty::Running`'s field is private to that
/// module, so this test owns its guard; everything else comes from `pty`.
pub(super) struct Spawned(Child);

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Tokens that stay fresh, so the daemon never reaches `rail_env`'s dead token endpoint
/// (`cli/sync.rs`'s form, not `store_work_tokens`, whose `expires_at` is 1).
pub(super) fn store_tokens(dir: &tempfile::TempDir) {
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

/// One unread inbox message `id` in its own thread `t-{id}`, received yesterday.
fn message(id: &str) -> Value {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    json!({
        "id": id, "threadId": format!("t-{id}"), "labelIds": ["INBOX", "UNREAD"], "snippet": "",
        "historyId": "10", "internalDate": ((now - 86_400) * 1000).to_string(),
        "sizeEstimate": 10,
        "payload": {"mimeType": "text/plain", "headers": [
            {"name": "Subject", "value": format!("Budget {id}")},
            {"name": "From", "value": "Dana <dana@example.com>"}
        ], "body": {"size": 5, "data": "aGVsbG8"}}
    })
}

/// Fake Gmail with `MESSAGES` in the inbox, each thread's `modify` answered: the daemon fills
/// the store from here, as `rail_sync_fills_the_store_and_exits_0_when_caught_up` does.
pub(super) async fn gmail() -> MockServer {
    let server = MockServer::start().await;
    let ok = |body: Value| ResponseTemplate::new(200).set_body_json(body);
    let listing =
        json!({"messages": MESSAGES.map(|id| json!({"id": id, "threadId": format!("t-{id}")}))});
    Mock::given(path(format!("{API}/labels")))
        .respond_with(ok(
            json!({"labels": [{"id": "INBOX", "name": "INBOX", "type": "system"}]}),
        ))
        .mount(&server)
        .await;
    Mock::given(path(format!("{API}/profile")))
        .respond_with(ok(json!({"emailAddress": "me@example.com", "messagesTotal": 3, "threadsTotal": 3, "historyId": "10"})))
        .mount(&server)
        .await;
    Mock::given(path(format!("{API}/history")))
        .respond_with(ok(json!({"history": [], "historyId": "10"})))
        .mount(&server)
        .await;
    for spam_trash in ["true", "false"] {
        Mock::given(method("GET"))
            .and(path(format!("{API}/messages")))
            .and(query_param("includeSpamTrash", spam_trash))
            .respond_with(ok(listing.clone()))
            .mount(&server)
            .await;
    }
    for id in MESSAGES {
        Mock::given(path(format!("{API}/messages/{id}")))
            .respond_with(ok(message(id)))
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(format!("{API}/threads/t-{id}/modify")))
            .respond_with(ok(json!({"id": format!("t-{id}")})))
            .mount(&server)
            .await;
    }
    server
}

/// The bodies of the `threads/{id}/modify` calls the provider took, by thread id.
async fn modifies(server: &MockServer) -> Vec<(String, Value)> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/modify"))
        .map(|r| {
            let thread = r
                .url
                .path()
                .trim_end_matches("/modify")
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_string();
            (
                thread,
                serde_json::from_slice(&r.body).unwrap_or(Value::Null),
            )
        })
        .collect()
}

/// Poll the provider until it has taken `count` `modify` calls. `LIMIT` does not govern an
/// `await`, so the poll is bounded here and fails with the screen as `wait_until` does.
pub(super) async fn wait_for_modifies(
    server: &MockServer,
    output: &Mutex<Vec<u8>>,
    count: usize,
) -> Vec<(String, Value)> {
    let polling = async {
        loop {
            let taken = modifies(server).await;
            if taken.len() >= count {
                return taken;
            }
            tokio::time::sleep(POLL).await;
        }
    };
    match tokio::time::timeout(LIMIT, polling).await {
        Ok(taken) => taken,
        Err(_) => panic!(
            "timed out waiting for the archive calls; screen:\n{:?}",
            text(output)
        ),
    }
}

/// `rail` in a 100x30 pseudo-terminal against `server`, with this test's own directories: the
/// child, its keyboard and everything it draws.
pub(super) fn launch(
    dir: &tempfile::TempDir,
    server: &MockServer,
) -> (Spawned, ChildStdin, Arc<Mutex<Vec<u8>>>) {
    let command = format!(
        "stty cols 100 rows 30; exec '{}'",
        env!("CARGO_BIN_EXE_rail")
    );
    let mut script = Command::new("script");
    // Overrides `rail_env`'s dead port for the binary and the daemon it starts.
    rail_env(&mut script, dir)
        .env("RAIL_TEST_GMAIL_URL", format!("{}{API}/", server.uri()))
        .args(["-q", "-e", "-c", &command, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Spawned(script.spawn().unwrap());
    let keys = child.0.stdin.take().unwrap();
    let output = collect(child.0.stdout.take().unwrap());
    (child, keys, output)
}

/// Wait until all three threads are drawn in the list.
pub(super) fn wait_for_the_list(output: &Arc<Mutex<Vec<u8>>>) {
    let drawn = output.clone();
    wait_until("the three threads in the list", output, move || {
        let shown = text(&drawn);
        MESSAGES
            .iter()
            .all(|id| shown.contains(&format!("Budget {id}")))
    });
}

/// Type `bytes` at the keyboard.
pub(super) fn type_keys(keys: &mut ChildStdin, bytes: &[u8]) {
    keys.write_all(bytes).unwrap();
    keys.flush().unwrap();
}

/// Wait for the child to exit; on timeout fail with what the terminal showed.
pub(super) fn wait_for_exit(
    child: &mut Spawned,
    output: &Mutex<Vec<u8>>,
) -> std::process::ExitStatus {
    let deadline = Instant::now() + LIMIT;
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "rail did not quit:\n{:?}",
            text(output)
        );
        std::thread::sleep(POLL);
    }
}

/// `wait_until` blocks its thread, so a second worker keeps the mock and the poll running.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn select_all_then_archive_reaches_the_provider() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = gmail().await;
    let (mut child, mut keys, output) = launch(&dir, &server);
    wait_for_the_list(&output);
    type_keys(&mut keys, b"*a");
    // The `Matched` answer is asynchronous: an `e` sent before the count is drawn would
    // archive the cursor row alone.
    let counted = output.clone();
    wait_until("`3 selected` in the header", &output, move || {
        drawn_row(&text(&counted), "3 selected") == Some(1)
    });
    // Three is under the default `bulk_confirm` of 50, so nothing is asked and no `y` is sent.
    type_keys(&mut keys, b"e");

    let taken = wait_for_modifies(&server, &output, MESSAGES.len()).await;
    let mut threads: Vec<String> = taken.iter().map(|(t, _)| t.clone()).collect();
    threads.sort();
    assert_eq!(
        threads,
        MESSAGES.map(|id| format!("t-{id}")).to_vec(),
        "one archive call per selected thread"
    );
    for (thread, body) in &taken {
        assert_eq!(
            body["removeLabelIds"],
            json!(["INBOX"]),
            "{thread}: Gmail's archive"
        );
    }

    type_keys(&mut keys, b"q");
    let status = wait_for_exit(&mut child, &output);
    drop(keys);
    std::thread::sleep(Duration::from_millis(200));
    let out = text(&output);
    assert!(status.success(), "{status:?}: {out:?}");
    let entered = out
        .find("\x1b[?1049h")
        .expect("entered the alternate screen");
    let left = out.rfind("\x1b[?1049l").expect("left the alternate screen");
    assert!(entered < left, "the terminal is restored: {out:?}");
}

/// `J` anchors on the top row and takes the next one with it; `e` then archives exactly those
/// two threads on Gmail, and the third is never touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_shift_range_archives_exactly_the_threads_it_covers() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = gmail().await;
    let (mut child, mut keys, output) = launch(&dir, &server);
    wait_for_the_list(&output);
    // The list's top two rows, as drawn.
    let shown = text(&output);
    let mut rows: Vec<(u16, String)> = MESSAGES
        .iter()
        .map(|id| {
            (
                drawn_row(&shown, &format!("Budget {id}")).unwrap(),
                format!("t-{id}"),
            )
        })
        .collect();
    rows.sort();
    let mut top_two: Vec<String> = rows[..2].iter().map(|(_, t)| t.clone()).collect();
    top_two.sort();
    type_keys(&mut keys, b"J");
    let counted = output.clone();
    wait_until("`2 selected` in the header", &output, move || {
        drawn_row(&text(&counted), "2 selected") == Some(1)
    });
    type_keys(&mut keys, b"e");
    let taken = wait_for_modifies(&server, &output, 2).await;
    let mut threads: Vec<String> = taken.iter().map(|(t, _)| t.clone()).collect();
    threads.sort();
    assert_eq!(threads, top_two, "the range's two threads");
    // Same grace, same worker turn: the third would have come with them.
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(modifies(&server).await.len(), 2, "the third thread stays");
    type_keys(&mut keys, b"q");
    let status = wait_for_exit(&mut child, &output);
    assert!(status.success(), "{status:?}: {:?}", text(&output));
}

/// `z` right after a bulk archive takes it back before the grace ends: the bar says so, and
/// Gmail never hears of it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn z_takes_back_a_bulk_archive_before_it_reaches_gmail() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = gmail().await;
    let (mut child, mut keys, output) = launch(&dir, &server);
    wait_for_the_list(&output);
    type_keys(&mut keys, b"*a");
    let counted = output.clone();
    wait_until("`3 selected` in the header", &output, move || {
        drawn_row(&text(&counted), "3 selected") == Some(1)
    });
    type_keys(&mut keys, b"e");
    let archived = output.clone();
    wait_until("the archive's notice", &output, move || {
        text(&archived).contains("archived 3")
    });
    type_keys(&mut keys, b"z");
    let undone = output.clone();
    wait_until("`undone` on the bar", &output, move || {
        text(&undone).contains("undone")
    });
    tokio::time::sleep(PAST_THE_GRACE).await;
    assert!(modifies(&server).await.is_empty(), "Gmail was never told");
    type_keys(&mut keys, b"q");
    let status = wait_for_exit(&mut child, &output);
    assert!(status.success(), "{status:?}: {:?}", text(&output));
}
