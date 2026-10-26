//! `h` in the real binary, in a pseudo-terminal, against fake Gmail: the thread is snoozed until
//! tomorrow, its archive reaches Gmail once the undo grace has passed, and Rail quits cleanly.

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::pty::{Running, collect, script_available, text, wait_for_exit, wait_until};
use super::sync::{API, gmail, store_tokens};
use super::{StopsDaemon, WORK_FILE_BACKEND, rail_env, write_config};

/// The undo grace, the worker's wait and a margin.
const ARCHIVE_WITHIN: Duration = Duration::from_secs(45);

/// The body of the first `threads/t-m1/modify` Gmail received.
async fn archive_of_t_m1(server: &MockServer) -> Value {
    let wanted = format!("{API}/threads/t-m1/modify");
    let deadline = Instant::now() + ARCHIVE_WITHIN;
    loop {
        let requests = server.received_requests().await.unwrap_or_default();
        if let Some(request) = requests.iter().find(|r| r.url.path() == wanted) {
            return serde_json::from_slice(&request.body).unwrap();
        }
        assert!(Instant::now() < deadline, "Gmail never got the archive");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn h_then_tomorrow_archives_the_thread_on_gmail_after_the_grace_and_rail_quits() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    store_tokens(&dir);
    let server = gmail().await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/threads/t-m1/modify")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "t-m1"})))
        .mount(&server)
        .await;
    let command = format!(
        "stty cols 100 rows 30; exec '{}'",
        env!("CARGO_BIN_EXE_rail")
    );
    let mut script = Command::new("script");
    rail_env(&mut script, &dir)
        .env("RAIL_TEST_GMAIL_URL", format!("{}{API}/", server.uri()))
        .args(["-q", "-e", "-c", &command, "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut child = Running(script.spawn().unwrap());
    let mut keys = child.0.stdin.take().unwrap();
    let output = collect(child.0.stdout.take().unwrap());
    let shown = output.clone();
    wait_until("the thread in the list", &output, move || {
        text(&shown).contains("Budget for Q3")
    });
    keys.write_all(b"h").unwrap();
    keys.flush().unwrap();
    let shown = output.clone();
    wait_until("the chooser", &output, move || {
        text(&shown).contains("Snooze until")
    });
    keys.write_all(b"tomorrow\r").unwrap();
    keys.flush().unwrap();
    let archive = archive_of_t_m1(&server).await;
    assert_eq!(archive["removeLabelIds"], json!(["INBOX"]), "{archive}");
    keys.write_all(b"q").unwrap();
    keys.flush().unwrap();
    let status = wait_for_exit(&mut child, &output);
    assert!(status.success(), "{:?}", text(&output));
}
