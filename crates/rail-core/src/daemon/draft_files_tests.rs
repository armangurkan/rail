//! The files an imported draft owns: fetched once at the import (0600, in a 0700 directory
//! named by the import's token), carried by every later push even once Gmail's copy of the
//! source message is gone, removed with the draft, and swept at daemon start when no draft
//! names them. A failed fetch leaves nothing behind.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use super::draft_gmail_tests::{PLAN, with_gmail_drafts};
use super::draft_tests::DaemonHarness;
use super::handlers::draft_files::{file_name, owned_dirs};
use crate::paths::Paths;
use crate::store::Store;
use crate::store::drafts::Attachment;

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

#[tokio::test]
async fn discarding_an_imported_draft_removes_its_files() {
    let h = with_gmail_drafts().await;
    let id = h.import(h.message_id("d1"), "i1").await.unwrap();
    let dir = h.paths.draft_files_dir().join("i1");
    assert!(dir.join("plan.pdf").exists());
    h.discard(id).await.unwrap();
    assert!(!dir.exists(), "the draft's directory went with it");
    assert!(h.paths.draft_files_dir().exists(), "only the draft's own");
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

#[test]
fn only_files_under_the_draft_files_directory_are_owned() {
    let paths = Paths::all_in(Path::new("/x"));
    let root = paths.draft_files_dir();
    let file = |p: &Path| Attachment::File {
        path: p.to_str().unwrap().into(),
        name: "n".into(),
        mime: "m".into(),
        size: 1,
    };
    let attachments = [
        file(&root.join("t1").join("a.pdf")),
        file(&root.join("t1").join("b.pdf")),
        file(Path::new("/home/me/c.pdf")),
        file(&root.join("t2").join("..").join("..").join("d.pdf")),
        file(&root.join("e.pdf")),
        file(&root.join("t3").join("deep").join("f.pdf")),
    ];
    assert_eq!(owned_dirs(&paths, &attachments), [root.join("t1")]);
}
