//! Saving an attachment through the real binary in a pseudo-terminal (AC-44): a thread opened
//! with `Enter`, its attachments listed with `a`, `s` on the first one, and the bytes the mocked
//! Gmail served landing as a 0600 file in the configured `attachment_dir`.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::pty::{LIMIT, collect, script_available, text, wait_until};
use super::{KillsChild, StopsDaemon, WORK_FILE_BACKEND, rail_env, write_config};

const API: &str = "/gmail/v1/users/me";
/// `The plan is ready.` in base64, as Gmail serves a text part.
const BODY_DATA: &str = "VGhlIHBsYW4gaXMgcmVhZHku";
/// `hello` in base64url, the attachment's five bytes.
const PLAN_DATA: &str = "aGVsbG8";

/// Tokens that stay fresh, so the run never refreshes against the dead token URL.
fn store_fresh_tokens(dir: &tempfile::TempDir) {
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

/// A store for `WORK_FILE_BACKEND`'s account holding one inbox thread whose body carries
/// `plan.pdf` (part `0.1`).
fn seed_thread_with_an_attachment(dir: &tempfile::TempDir) {
    use rail_core::provider::{AttachmentMeta, Body, Envelope, FolderKind, Mailbox, RemoteFolder};
    use rail_core::store::Store;
    use rail_core::sync::Batch;
    let data = dir.path().join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut store = Store::open(&data.join("rail.db")).unwrap();
    store
        .conn()
        .execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('work', 'gmail', 'me@example.com')",
            [],
        )
        .unwrap();
    let envelope = Envelope {
        remote_id: "m1".into(),
        thread_id: "t1".into(),
        subject: "Quarterly plan".into(),
        from: vec![Mailbox {
            name: Some("Tom".into()),
            address: "tom@example.com".into(),
        }],
        date: 1_760_000_000,
        internal_date: 1_760_000_000_000,
        folders: vec!["INBOX".into()],
        ..Envelope::default()
    };
    let body = Body {
        text: "The plan is ready.".into(),
        attachments: vec![AttachmentMeta {
            part_id: "0.1".into(),
            filename: "plan.pdf".into(),
            mime_type: "application/pdf".into(),
            size: 5,
            plain_file: true,
        }],
        ..Body::default()
    };
    let batch = Batch {
        folders: Some(vec![RemoteFolder {
            parent: None,
            remote_id: "INBOX".into(),
            name: "Inbox".into(),
            kind: FolderKind::Inbox,
        }]),
        upserts: vec![envelope],
        bodies: vec![("m1".into(), body)],
        ..Batch::new(1)
    };
    store.apply(&batch, 1_760_000_000).unwrap();
}

/// Fake Gmail: enough for the daemon's start-up sync, plus m1 as a `multipart/mixed` message
/// whose child part is the attachment, its five bytes inline.
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
        .respond_with(ok(
            json!({"emailAddress": "me@example.com", "messagesTotal": 1, "threadsTotal": 1, "historyId": "10"}),
        ))
        .mount(&server)
        .await;
    Mock::given(path(format!("{API}/history")))
        .respond_with(ok(json!({"history": [], "historyId": "10"})))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages")))
        .respond_with(ok(json!({"messages": [{"id": "m1", "threadId": "t1"}]})))
        .mount(&server)
        .await;
    Mock::given(path(format!("{API}/messages/m1")))
        .respond_with(ok(json!({
            "id": "m1", "threadId": "t1", "labelIds": ["INBOX"], "snippet": "",
            "historyId": "10", "internalDate": "1760000000000", "sizeEstimate": 40,
            "payload": {
                "mimeType": "multipart/mixed",
                "headers": [
                    {"name": "Subject", "value": "Quarterly plan"},
                    {"name": "From", "value": "Tom <tom@example.com>"}
                ],
                "parts": [
                    {"partId": "0", "mimeType": "text/plain", "filename": "",
                     "body": {"size": 18, "data": BODY_DATA}},
                    {"partId": "0.1", "filename": "plan.pdf", "mimeType": "application/pdf",
                     "body": {"size": 5, "data": PLAN_DATA}}
                ]
            }
        })))
        .mount(&server)
        .await;
    server
}

/// AC-44: `a` then `s` in the reader saves the attachment into `attachment_dir`.
#[tokio::test]
async fn pressing_a_then_s_in_the_reader_saves_the_attachment_to_the_configured_directory() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let saved = dir.path().join("saved");
    let config = WORK_FILE_BACKEND.replacen(
        "secrets = \"file\"\n",
        &format!(
            "secrets = \"file\"\nattachment_dir = \"{}\"\n",
            saved.display()
        ),
        1,
    );
    write_config(&dir, &config);
    store_fresh_tokens(&dir);
    seed_thread_with_an_attachment(&dir);
    let server = gmail().await;

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
    let mut child = KillsChild(Some(script.spawn().unwrap()));
    let running = child.0.as_mut().unwrap();
    let mut keys = running.stdin.take().unwrap();
    let output = collect(running.stdout.take().unwrap());
    let seen = |needle: &'static str| {
        let output = output.clone();
        move || text(&output).contains(needle)
    };
    let mut press = |bytes: &[u8]| {
        keys.write_all(bytes).unwrap();
        keys.flush().unwrap();
    };

    wait_until("the thread in the list", &output, seen("Quarterly plan"));
    press(b"\r");
    wait_until("the reader", &output, seen("ready."));
    press(b"a");
    wait_until("the attachment list", &output, seen("Attachments"));
    press(b"s");
    let file = saved.join("plan.pdf");
    wait_until("the saved file", &output, || file.exists());
    assert_eq!(std::fs::read(&file).unwrap(), b"hello", "the served bytes");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600,
        "owner-only"
    );
    wait_until("the status line", &output, seen("Saved plan.pdf to"));

    press(b"q");
    let status = {
        let running = child.0.as_mut().unwrap();
        let deadline = Instant::now() + LIMIT;
        loop {
            if let Some(status) = running.try_wait().unwrap() {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "rail did not quit:\n{:?}",
                text(&output)
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    assert!(status.success(), "{status:?}: {:?}", text(&output));
}
