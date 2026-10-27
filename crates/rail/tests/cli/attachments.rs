//! Saving and opening attachments through the real binary in a pseudo-terminal, against a
//! mocked Gmail: `a` then `s` on one part (AC-44), and `w` and `S` on every
//! part of a message. Every wait reads the screen a 100x30 terminal would show — Rail redraws only the
//! cells that changed, so a phrase on the bar is never one run in the stream — and every test
//! checks the files on disk, their modes, and what Gmail was asked for. Linux only, like every
//! pty test.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::ChildStdin;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::bulk;
use super::pty::{screen, script_available, text, wait_until};
use super::{StopsDaemon, WORK_FILE_BACKEND, write_config};

const API: &str = "/gmail/v1/users/me";
/// `The plan is ready.` in base64, as Gmail serves a text part.
const BODY_DATA: &str = "VGhlIHBsYW4gaXMgcmVhZHku";
/// `hello` in base64url, `plan.pdf`'s five bytes.
const PLAN_DATA: &str = "aGVsbG8";
/// `the notes` in base64url, `notes.txt`'s nine.
const NOTES_DATA: &str = "dGhlIG5vdGVz";
/// The attachment list's top border, as `boxed` draws it.
const BOX: &str = "┌ Attachments ";
/// The hint bar, the last row of the 100x30 screen.
const BAR: usize = 29;

/// One attachment of the seeded message, as both the store and fake Gmail describe it.
struct Part {
    part_id: &'static str,
    filename: &'static str,
    mime_type: &'static str,
    /// The bytes Gmail serves, and `data` is those bytes in base64url.
    bytes: &'static [u8],
    data: &'static str,
    /// Gmail's own id for the bytes when they arrive from `attachments.get` instead of inline
    /// in the message. A fetch of that path is a request only a save or an open makes: the
    /// sync reads a message's parts from the message itself.
    attachment_id: Option<&'static str>,
}

/// `plan.pdf`, five bytes inline: AC-44's part, byte for byte as it was.
const PLAN: Part = Part {
    part_id: "0.1",
    filename: "plan.pdf",
    mime_type: "application/pdf",
    bytes: b"hello",
    data: PLAN_DATA,
    attachment_id: None,
};

/// `notes.txt`, whose bytes Gmail only hands over from `attachments.get`.
const NOTES: Part = Part {
    part_id: "0.2",
    filename: "notes.txt",
    mime_type: "text/plain",
    bytes: b"the notes",
    data: NOTES_DATA,
    attachment_id: Some("att-notes"),
};

impl Part {
    /// The part as Gmail serves it inside the message.
    fn wire(&self) -> Value {
        let size = self.bytes.len();
        let body = match self.attachment_id {
            None => json!({"size": size, "data": self.data}),
            Some(id) => json!({"size": size, "attachmentId": id}),
        };
        json!({
            "partId": self.part_id, "filename": self.filename,
            "mimeType": self.mime_type, "body": body
        })
    }

    /// The part as the store lists it.
    fn meta(&self) -> rail_core::provider::AttachmentMeta {
        rail_core::provider::AttachmentMeta {
            part_id: self.part_id.into(),
            filename: self.filename.into(),
            mime_type: self.mime_type.into(),
            size: i64::try_from(self.bytes.len()).unwrap(),
            plain_file: true,
        }
    }
}

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
/// `parts`.
fn seed_thread_with_attachments(dir: &tempfile::TempDir, parts: &[Part]) {
    use rail_core::provider::{Body, Envelope, FolderKind, Mailbox, RemoteFolder};
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
        attachments: parts.iter().map(Part::meta).collect(),
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

/// A 200 with a JSON body.
fn ok(body: Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(body)
}

/// The routes the daemon's start-up sync asks for before any thread is opened.
async fn mount_the_sync(server: &MockServer) {
    Mock::given(path(format!("{API}/labels")))
        .respond_with(ok(
            json!({"labels": [{"id": "INBOX", "name": "INBOX", "type": "system"}]}),
        ))
        .mount(server)
        .await;
    Mock::given(path(format!("{API}/profile")))
        .respond_with(ok(
            json!({"emailAddress": "me@example.com", "messagesTotal": 1, "threadsTotal": 1, "historyId": "10"}),
        ))
        .mount(server)
        .await;
    Mock::given(path(format!("{API}/history")))
        .respond_with(ok(json!({"history": [], "historyId": "10"})))
        .mount(server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API}/messages")))
        .respond_with(ok(json!({"messages": [{"id": "m1", "threadId": "t1"}]})))
        .mount(server)
        .await;
}

/// Fake Gmail: the start-up sync's routes, plus m1 as a `multipart/mixed` message whose child
/// parts are `parts` — each one's bytes inline, or behind the `attachments.get` path its
/// `attachment_id` names.
async fn gmail(parts: &[Part]) -> MockServer {
    let server = MockServer::start().await;
    mount_the_sync(&server).await;
    let mut children = vec![json!({
        "partId": "0", "mimeType": "text/plain", "filename": "",
        "body": {"size": 18, "data": BODY_DATA}
    })];
    for part in parts {
        children.push(part.wire());
        if let Some(id) = part.attachment_id {
            Mock::given(path(format!("{API}/messages/m1/attachments/{id}")))
                .respond_with(ok(json!({"size": part.bytes.len(), "data": part.data})))
                .mount(&server)
                .await;
        }
    }
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
                "parts": children
            }
        })))
        .mount(&server)
        .await;
    server
}

/// One run of the binary: the live child, its keyboard, the screen it has drawn, the Gmail it
/// talks to and the directory `attachment_dir` points at. The daemon is stopped by the
/// caller's own `StopsDaemon`.
struct Run {
    child: bulk::Spawned,
    keys: ChildStdin,
    output: Arc<Mutex<Vec<u8>>>,
    server: MockServer,
    saved: PathBuf,
}

/// Write the config, the tokens and the thread, start fake Gmail and launch `rail` at 100x30
/// with `attachment_dir` pointing at `<dir>/saved`.
async fn start(dir: &tempfile::TempDir, parts: &[Part]) -> Run {
    let saved = dir.path().join("saved");
    let config = WORK_FILE_BACKEND.replacen(
        "secrets = \"file\"\n",
        &format!(
            "secrets = \"file\"\nattachment_dir = \"{}\"\n",
            saved.display()
        ),
        1,
    );
    write_config(dir, &config);
    store_fresh_tokens(dir);
    seed_thread_with_attachments(dir, parts);
    let server = gmail(parts).await;
    let (child, keys, output) = bulk::launch(dir, &server);
    Run {
        child,
        keys,
        output,
        server,
        saved,
    }
}

/// Wait until the screen, as a 100x30 terminal shows it, passes `check` (rows from 0). The
/// pattern `cli/snooze.rs` follows, and the reason `pty::screen` exists: a redraw touches only
/// the cells that changed, so the stream carries fragments and cursor moves, never the phrase.
fn wait_shown(
    what: &str,
    output: &Arc<Mutex<Vec<u8>>>,
    check: impl Fn(&[String]) -> bool + Send + 'static,
) {
    let drawn = output.clone();
    wait_until(what, output, move || check(&screen(&text(&drawn), 100, 30)));
}

/// Wait until row `row` (from 0) shows `needle`.
fn wait_row(what: &str, output: &Arc<Mutex<Vec<u8>>>, row: usize, needle: String) {
    wait_shown(what, output, move |rows| rows[row].contains(&needle));
}

/// Wait until some row shows `needle`.
fn wait_anywhere(what: &str, output: &Arc<Mutex<Vec<u8>>>, needle: &'static str) {
    wait_shown(what, output, move |rows| {
        rows.iter().any(|r| r.contains(needle))
    });
}

/// Open the thread and wait for the reader. The one barrier a run needs before the attachment
/// keys: the thread's messages come from the daemon, while `a` reads what the reader already
/// holds. Keys after this are written together — the TUI reads them in order, each in the
/// state the one before it left.
fn open_the_thread(run: &mut Run) {
    wait_anywhere("the thread in the list", &run.output, "Quarterly plan");
    bulk::type_keys(&mut run.keys, b"\r");
    wait_anywhere("the reader", &run.output, "ready.");
}

/// `a` in the reader, with the list drawn.
fn open_the_list(run: &mut Run) {
    bulk::type_keys(&mut run.keys, b"a");
    wait_anywhere("the attachment list", &run.output, BOX);
}

/// Send `keys` and wait for Rail to end successfully.
fn ends(run: &mut Run, keys: &[u8]) {
    bulk::type_keys(&mut run.keys, keys);
    let status = bulk::wait_for_exit(&mut run.child, &run.output);
    assert!(status.success(), "{status:?}: {:?}", text(&run.output));
}

/// What `path` holds and the mode it holds it at.
fn saved_file(path: &Path) -> (Vec<u8>, u32) {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("{path:?}: {error}"));
    let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    (bytes, mode)
}

/// Every part of `parts` on disk in `dir`, owner-only, holding the bytes Gmail served.
fn each_part_saved(dir: &Path, parts: &[Part]) {
    for part in parts {
        let (bytes, mode) = saved_file(&dir.join(part.filename));
        assert_eq!(bytes, part.bytes, "{}: the served bytes", part.filename);
        assert_eq!(mode, 0o600, "{}: owner-only", part.filename);
    }
}

/// How many times Gmail was asked for `attachment_id`'s bytes.
async fn attachment_fetches(server: &MockServer, attachment_id: &str) -> usize {
    let wanted = format!("{API}/messages/m1/attachments/{attachment_id}");
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| request.url.path() == wanted)
        .count()
}

/// How many `format=full` reads of m1 Gmail has answered: a part served inline costs one of
/// these, because Gmail's own attachment id is not stable and Rail re-reads the message.
async fn full_reads(server: &MockServer) -> usize {
    let wanted = format!("{API}/messages/m1");
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .filter(|request| {
            request.url.path() == wanted
                && request
                    .url
                    .query_pairs()
                    .any(|(key, value)| key == "format" && value == "full")
        })
        .count()
}

/// AC-44 (US-1): `a` then `s` in the reader saves the attachment into `attachment_dir`.
#[tokio::test]
async fn pressing_a_then_s_in_the_reader_saves_the_attachment_to_the_configured_directory() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let mut run = start(&dir, &[PLAN]).await;
    open_the_thread(&mut run);
    // Counted from here, so the read the save makes is told from the sync's own.
    let before = full_reads(&run.server).await;
    open_the_list(&mut run);
    bulk::type_keys(&mut run.keys, b"s");
    // The status line is the barrier, not `file.exists()`: the handler creates the file with
    // `create_new` before it writes and fsyncs it (§7), so a poll between the two would read a
    // short file. The status is printed only after the op answered, and the whole §8 row is
    // waited on over the rebuilt screen.
    wait_row(
        "the save's status line",
        &run.output,
        BAR,
        format!("Saved plan.pdf to {}", run.saved.display()),
    );
    let file = run.saved.join("plan.pdf");
    assert_eq!(std::fs::read(&file).unwrap(), b"hello", "the served bytes");
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600,
        "owner-only"
    );
    assert_eq!(
        full_reads(&run.server).await,
        before + 1,
        "Gmail was read once more, for the bytes"
    );
    // `s` closes the list and leaves the reader open, where `q` is `reader.back` (a view's own
    // binding beats `Global`), so the first `q` closes the thread and the second quits.
    // Measured on Linux at this anchor: after the first `q` alone the process was still running.
    ends(&mut run, b"qq");
}

/// US-2, FR-9: `w` in the reader saves every attachment of the message under the cursor,
/// without opening the list, and the bar carries §8's save-all line.
#[tokio::test]
async fn w_in_the_reader_saves_every_attachment_of_the_message() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let parts = [PLAN, NOTES];
    let mut run = start(&dir, &parts).await;
    open_the_thread(&mut run);
    bulk::type_keys(&mut run.keys, b"w");
    wait_row(
        "the save-all line",
        &run.output,
        BAR,
        format!("Saved 2 attachments to {}", run.saved.display()),
    );
    each_part_saved(&run.saved, &parts);
    assert_eq!(
        attachment_fetches(&run.server, "att-notes").await,
        1,
        "Gmail served notes.txt's bytes once"
    );
    wait_shown("no list was opened", &run.output, |rows| {
        rows.iter().all(|row| !row.contains(BOX))
    });
    ends(&mut run, b"qq");
}

/// US-2, FR-4: `S` in the attachment list saves every attachment of that row's message and
/// closes the list.
#[tokio::test]
async fn s_capital_in_the_list_saves_every_attachment() {
    if !script_available() {
        eprintln!("skipping: util-linux `script` is not installed");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let _daemon = StopsDaemon(&dir);
    let parts = [PLAN, NOTES];
    let mut run = start(&dir, &parts).await;
    open_the_thread(&mut run);
    open_the_list(&mut run);
    bulk::type_keys(&mut run.keys, b"S");
    wait_row(
        "the save-all line",
        &run.output,
        BAR,
        format!("Saved 2 attachments to {}", run.saved.display()),
    );
    wait_shown("the list closed", &run.output, |rows| {
        rows.iter().all(|row| !row.contains(BOX))
    });
    each_part_saved(&run.saved, &parts);
    assert_eq!(
        attachment_fetches(&run.server, "att-notes").await,
        1,
        "Gmail served notes.txt's bytes once"
    );
    ends(&mut run, b"qq");
}
