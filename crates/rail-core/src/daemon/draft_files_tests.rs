//! The files an imported draft owns: fetched once at the import (0600, in a 0700 directory
//! named by the import's token), carried by every later push even once Gmail's copy of the
//! source message is gone, removed with the draft, and swept at daemon start when no draft
//! names them. A failed fetch leaves nothing behind.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::draft_gmail_tests::{PLAN, seed_drafts, with_gmail_drafts};
use super::draft_tests::{DaemonHarness, WORK};
use super::handlers::draft_files::file_name;
use crate::ops::DraftImportRequest;
use crate::ops::op::DraftImport;
use crate::paths::Paths;
use crate::provider::AttachmentMeta;
use crate::store::Store;
use crate::store::drafts::Attachment;

const HOME: &str = "\n[[accounts]]\nname = \"home\"\nkind = \"gmail\"\naddress = \"home@example.com\"\nclient_id = \"cid2\"\n";

/// `PLAN` as the MIME builder encodes it.
const PLAN_BASE64: &str = "JVBERi0xLjQgdGhlIHBsYW4=";

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

impl DaemonHarness {
    /// Push `draft` now and wait for the worker to finish it.
    async fn pushed(&self, draft: i64, token: &str) {
        let row = self.push(draft, token).await.unwrap();
        self.due_now(row).await;
        self.until("the push landed", |h| {
            h.store(|s| s.pending_rows_for_draft(draft).unwrap())
                .is_empty()
        })
        .await;
        let account = self.work_id();
        assert!(
            self.store(|s| s.failed_rows(account).unwrap()).is_empty(),
            "the push failed"
        );
    }

    /// The store's id of the message Gmail calls `remote` in the account `name`.
    fn message_in(&self, name: &str, remote: &str) -> Option<i64> {
        self.store(|s| {
            s.conn()
                .query_row(
                    "SELECT m.id FROM messages m JOIN accounts a ON a.id = m.account_id
                     WHERE a.name = ?1 AND m.remote_id = ?2",
                    [name, remote],
                    |r| r.get(0),
                )
                .ok()
        })
    }

    fn gmail_message(&self, draft: &str) -> String {
        self.mock.with(|m| m.drafts[draft].message_id.clone())
    }

    fn gmail_raw(&self, draft: &str) -> String {
        self.mock
            .with(|m| String::from_utf8_lossy(&m.drafts[draft].raw).into_owned())
    }
}

#[tokio::test]
async fn an_imported_attachment_is_a_private_file_with_its_bytes() {
    let h = with_gmail_drafts().await;
    let id = h.import(h.message_id("d1"), "i1").await.unwrap();
    let dir = h.paths.draft_files_dir().join("i1");
    let d = h.store(|s| s.draft(id).unwrap().unwrap());
    let [Attachment::File { path, size, .. }] = d.attachments.as_slice() else {
        panic!("one file attachment");
    };
    assert_eq!(Path::new(path), dir.join("plan.pdf"));
    assert_eq!(std::fs::read(path).unwrap(), PLAN);
    assert_eq!(*size, i64::try_from(PLAN.len()).unwrap());
    assert_eq!(mode(Path::new(path)), 0o600);
    assert_eq!(mode(&dir), 0o700);
}

/// The ruling's case: the first push replaces Gmail's message, a sync deletes it from the
/// store, and the second push still carries the attachment.
#[tokio::test]
async fn a_second_push_after_the_source_message_is_gone_carries_the_attachment() {
    let h = with_gmail_drafts().await;
    let id = h.import(h.message_id("d1"), "i1").await.unwrap();
    h.pushed(id, "p1").await;
    assert!(h.gmail_raw("r1").contains(PLAN_BASE64), "the first push");
    Store::open(&h.paths.db_file())
        .unwrap()
        .conn()
        .execute_batch(
            "DELETE FROM attachments WHERE message_id IN
               (SELECT id FROM messages WHERE remote_id = 'd1');
             DELETE FROM messages WHERE remote_id = 'd1';",
        )
        .unwrap();
    h.mock.with(|m| m.attachment_bytes.clear());
    let before = h.gmail_message("r1");
    h.pushed(id, "p2").await;
    assert_ne!(h.gmail_message("r1"), before, "the second push updated r1");
    let raw = h.gmail_raw("r1");
    assert!(raw.contains(PLAN_BASE64), "the second push");
    assert!(raw.contains("plan.pdf"));
}

/// A discard removes the directory the draft's own import token names, and no other, even
/// one a file entry of the draft points into.
#[tokio::test]
async fn discarding_an_imported_draft_removes_its_files() {
    let h = with_gmail_drafts().await;
    let id = h.import(h.message_id("d1"), "i1").await.unwrap();
    let dir = h.paths.draft_files_dir().join("i1");
    assert!(dir.join("plan.pdf").exists());
    let other = h.paths.draft_files_dir().join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("f.pdf"), b"f").unwrap();
    let mut d = h.store(|s| s.draft(id).unwrap().unwrap());
    d.attachments.push(Attachment::File {
        path: other.join("f.pdf").to_str().unwrap().into(),
        name: "f.pdf".into(),
        mime: "application/pdf".into(),
        size: 1,
    });
    Store::open(&h.paths.db_file())
        .unwrap()
        .conn()
        .execute(
            "UPDATE drafts SET attachments_json = ?2 WHERE id = ?1",
            rusqlite::params![id, serde_json::to_string(&d.attachments).unwrap()],
        )
        .unwrap();
    h.discard(id).await.unwrap();
    assert!(!dir.exists(), "the draft's directory went with it");
    assert!(other.join("f.pdf").exists(), "only the draft's own");
}

/// Tokens are unique across accounts: another account's import with a token an imported
/// draft holds is refused and the first draft's files stay.
#[tokio::test]
async fn an_import_in_another_account_with_a_held_token_leaves_the_files_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), format!("{WORK}{HOME}")).unwrap();
    let mock = crate::provider::mock::MockProvider::new();
    seed_drafts(&mock);
    let h = DaemonHarness::boot(tmp, paths, mock).await;
    h.until("home synced", |h| h.message_in("home", "d1").is_some())
        .await;
    let work = h.message_in("work", "d1").unwrap();
    h.import(work, "T").await.unwrap();
    let file = h.paths.draft_files_dir().join("T").join("plan.pdf");
    let request = DraftImportRequest {
        account: "home".into(),
        message: h.message_in("home", "d1").unwrap(),
        client_token: "T".into(),
    };
    let error = h.call::<DraftImport>(&request).await.unwrap_err();
    assert_eq!(
        error.message,
        "client_token: this token was used for another action"
    );
    assert_eq!(
        std::fs::read(&file).unwrap(),
        PLAN,
        "the first draft's file"
    );
    assert_eq!(h.drafts_count(), 1);
}

/// Names that collide with a temp-file suffix, and one at the 255-byte limit, each keep
/// their own bytes.
#[tokio::test]
async fn attachments_named_like_temp_files_and_at_the_name_limit_import() {
    let h = with_gmail_drafts().await;
    let long = format!("{}.pdf", "n".repeat(251));
    let names = ["x.tmp".to_string(), "x".to_string(), long.clone()];
    h.mock.with(|m| {
        let body = m.bodies.get_mut("d1").unwrap();
        body.attachments = names
            .iter()
            .enumerate()
            .map(|(i, name)| AttachmentMeta {
                part_id: format!("q{i}"),
                filename: name.clone(),
                mime_type: "application/octet-stream".into(),
                size: 1,
            })
            .collect();
        for i in 0..names.len() {
            m.attachment_bytes.insert(
                ("d1".into(), format!("q{i}")),
                format!("bytes {i}").into_bytes(),
            );
        }
    });
    let message = h.message_id("d1");
    Store::open(&h.paths.db_file())
        .unwrap()
        .conn()
        .execute_batch(&format!(
            "UPDATE messages SET body_text = NULL, body_fetched_at = NULL WHERE id = {message};
             DELETE FROM attachments WHERE message_id = {message};"
        ))
        .unwrap();
    let id = h.import(message, "i1").await.unwrap();
    let d = h.store(|s| s.draft(id).unwrap().unwrap());
    assert_eq!(d.attachments.len(), 3);
    for (i, a) in d.attachments.iter().enumerate() {
        let Attachment::File { path, name, .. } = a else {
            panic!("a file entry");
        };
        assert_eq!(name, &names[i]);
        assert_eq!(
            std::fs::read(path).unwrap(),
            format!("bytes {i}").into_bytes()
        );
        assert_eq!(mode(Path::new(path)), 0o600);
    }
    assert_eq!(long.len(), 255);
    let dir = h.paths.draft_files_dir().join("i1");
    assert_eq!(
        std::fs::read_dir(dir).unwrap().count(),
        3,
        "no temp file left"
    );
}

#[tokio::test]
async fn the_start_sweep_removes_unreferenced_directories_only() {
    let h = with_gmail_drafts().await;
    h.import(h.message_id("d1"), "i1").await.unwrap();
    let root = h.paths.draft_files_dir();
    std::fs::create_dir_all(root.join("stale")).unwrap();
    std::fs::write(root.join("stale").join("old.pdf"), b"old").unwrap();
    let h = h.restart().await;
    h.until("the stale directory swept", |h| {
        !h.paths.draft_files_dir().join("stale").exists()
    })
    .await;
    assert!(
        root.join("i1").join("plan.pdf").exists(),
        "referenced, kept"
    );
}

#[tokio::test]
async fn a_failing_attachment_fetch_fails_the_import_and_leaves_nothing() {
    let h = with_gmail_drafts().await;
    h.mock.with(|m| m.attachment_bytes.clear());
    let error = h.import(h.message_id("d1"), "i1").await.unwrap_err();
    assert_eq!(
        error.code,
        crate::ops::ErrorCode::NotFound,
        "{}",
        error.message
    );
    assert!(!h.paths.draft_files_dir().join("i1").exists());
    assert_eq!(h.drafts_count(), 0);
}

#[test]
fn file_names_are_safe_and_distinct() {
    let mut taken = std::collections::HashSet::new();
    assert_eq!(file_name("plan.pdf", &mut taken), "plan.pdf");
    assert_eq!(file_name("Plan.PDF", &mut taken), "2-Plan.PDF");
    assert_eq!(file_name("../../plan.pdf", &mut taken), "3-plan.pdf");
    assert_eq!(file_name("..", &mut taken), "attachment");
    assert_eq!(file_name(".", &mut taken), "2-attachment");
    assert!(!file_name("a/b\\c", &mut taken).contains(['/', '\\']));
}

/// An import never clears a directory a draft names, whatever made it: refused, and the
/// file stays.
#[tokio::test]
async fn an_import_never_takes_a_directory_a_draft_names() {
    let h = with_gmail_drafts().await;
    let dir = h.paths.draft_files_dir().join("T2");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("mine.pdf"), b"mine").unwrap();
    let request = crate::ops::DraftSaveRequest {
        attachments: vec![crate::ops::DraftAttachment::File {
            path: dir.join("mine.pdf").to_str().unwrap().into(),
        }],
        ..super::draft_tests::save_request(None, "s1")
    };
    h.call::<crate::ops::op::DraftSave>(&request).await.unwrap();
    let error = h.import(h.message_id("d1"), "T2").await.unwrap_err();
    assert_eq!(
        error.message,
        "client_token: this token was used for another action"
    );
    assert_eq!(std::fs::read(dir.join("mine.pdf")).unwrap(), b"mine");
    assert_eq!(h.drafts_count(), 1);
}
