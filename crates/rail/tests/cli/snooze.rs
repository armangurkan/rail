//! `h` in the real binary, in a pseudo-terminal, against fake Gmail: the thread is snoozed until
//! tomorrow, its archive reaches Gmail once the undo grace has passed, and Rail quits cleanly.
//! A selection is snoozed the same way, counted in the chooser and on the bar, and "Return to
//! inbox now" from the Snoozed mailbox puts it back on Gmail.

use std::io::Write;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::bulk;
use super::pty::{
    LIMIT, Running, collect, screen, script_available, text, wait_for_exit, wait_until,
};
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

/// The header, the first row of the screen; the bar, the last.
const HEADER: usize = 0;
const BAR: usize = 29;

/// Wait until the screen, as a 100x30 terminal shows it, passes `check` (rows from 0).
fn wait_shown(
    what: &str,
    output: &Arc<Mutex<Vec<u8>>>,
    check: impl Fn(&[String]) -> bool + Send + 'static,
) {
    let drawn = output.clone();
    wait_until(what, output, move || check(&screen(&text(&drawn), 100, 30)));
}

/// Wait until row `row` (from 0) shows `needle`.
fn wait_row(what: &str, output: &Arc<Mutex<Vec<u8>>>, row: usize, needle: &'static str) {
    wait_shown(what, output, move |rows| rows[row].contains(needle));
}

/// Wait until some row shows `needle`.
fn wait_anywhere(what: &str, output: &Arc<Mutex<Vec<u8>>>, needle: &'static str) {
    wait_shown(what, output, move |rows| {
        rows.iter().any(|r| r.contains(needle))
    });
}

/// `* a`, then `h`: the chooser counts the three threads; `tomorrow`, Enter: the bar counts the
/// snooze.
fn snooze_all_until_tomorrow(keys: &mut ChildStdin, output: &Arc<Mutex<Vec<u8>>>) {
    bulk::wait_for_the_list(output);
    bulk::type_keys(keys, b"*a");
    wait_row("`3 selected` in the header", output, HEADER, "3 selected");
    bulk::type_keys(keys, b"h");
    wait_anywhere("the chooser's count", output, "Snooze 3 threads until");
    bulk::type_keys(keys, b"tomorrow\r");
    wait_row("the snooze's notice", output, BAR, "Snoozed 3 until");
}

/// The `messages/batchModify` calls Gmail took: their bodies.
async fn batch_modifies(server: &MockServer) -> Vec<Value> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/messages/batchModify"))
        .map(|r| serde_json::from_slice(&r.body).unwrap_or(Value::Null))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_selection_snoozed_with_h_is_counted_and_archived_on_gmail() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    bulk::store_tokens(&dir);
    let server = bulk::gmail().await;
    let (mut child, mut keys, output) = bulk::launch(&dir, &server);
    snooze_all_until_tomorrow(&mut keys, &output);
    let taken = bulk::wait_for_modifies(&server, &output, bulk::MESSAGES.len()).await;
    let mut threads: Vec<String> = taken.iter().map(|(t, _)| t.clone()).collect();
    threads.sort();
    assert_eq!(threads, bulk::MESSAGES.map(|id| format!("t-{id}")).to_vec());
    for (thread, body) in &taken {
        assert_eq!(
            body["removeLabelIds"],
            json!(["INBOX"]),
            "{thread}: the archive"
        );
    }
    bulk::type_keys(&mut keys, b"q");
    let status = bulk::wait_for_exit(&mut child, &output);
    assert!(status.success(), "{status:?}: {:?}", text(&output));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn return_to_inbox_now_puts_a_snoozed_selection_back_on_gmail() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    write_config(&dir, WORK_FILE_BACKEND);
    bulk::store_tokens(&dir);
    let server = bulk::gmail().await;
    Mock::given(method("POST"))
        .and(path(format!("{API}/messages/batchModify")))
        .respond_with(ResponseTemplate::new(204))
        .mount(&server)
        .await;
    let (mut child, mut keys, output) = bulk::launch(&dir, &server);
    snooze_all_until_tomorrow(&mut keys, &output);
    // The archives reached Gmail: the snoozes hold their threads.
    bulk::wait_for_modifies(&server, &output, bulk::MESSAGES.len()).await;
    bulk::type_keys(&mut keys, b":mailbox snoozed\r");
    wait_row("the Snoozed mailbox", &output, HEADER, "Snoozed");
    bulk::type_keys(&mut keys, b"*a");
    wait_row(
        "`3 selected` in the Snoozed mailbox",
        &output,
        HEADER,
        "3 selected",
    );
    bulk::type_keys(&mut keys, b"h");
    wait_anywhere("Return to inbox now", &output, "Return to inbox now");
    bulk::type_keys(&mut keys, b"\r");
    wait_row(
        "the Return's notice",
        &output,
        BAR,
        "returned 3 to the inbox",
    );
    // Each thread's snooze returns its own messages: one `batchModify` per thread.
    let deadline = Instant::now() + LIMIT;
    let backs = loop {
        let backs = batch_modifies(&server).await;
        let ids: usize = backs
            .iter()
            .map(|b| b["ids"].as_array().map_or(0, Vec::len))
            .sum();
        if ids >= bulk::MESSAGES.len() {
            break backs;
        }
        assert!(
            Instant::now() < deadline,
            "Gmail never got every return: {backs:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let mut ids: Vec<String> = Vec::new();
    for back in &backs {
        assert_eq!(back["addLabelIds"], json!(["INBOX"]), "{back}");
        ids.extend(serde_json::from_value::<Vec<String>>(back["ids"].clone()).unwrap());
    }
    ids.sort();
    assert_eq!(
        ids,
        bulk::MESSAGES.map(String::from).to_vec(),
        "every snoozed message, once"
    );
    wait_anywhere(
        "the Snoozed mailbox emptied",
        &output,
        "Nothing is snoozed.",
    );
    bulk::type_keys(&mut keys, b"q");
    let status = bulk::wait_for_exit(&mut child, &output);
    assert!(status.success(), "{status:?}: {:?}", text(&output));
}
