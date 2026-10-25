//! The quarantine attribute every file `attachment.save` and `attachment.open` writes carries
//! on macOS. Rail hands those files to a program of the system's choosing, so they are marked
//! as content that arrived from elsewhere rather than as files the user made. Linux has no
//! such attribute, so this module is macOS-only.

use std::path::{Path, PathBuf};

use std::os::unix::fs::PermissionsExt;

use super::super::draft_tests::DaemonHarness;
use crate::ops::op::{AttachmentOpen, AttachmentSave};
use crate::ops::{AttachmentOpenRequest, AttachmentOutcome, AttachmentSaveRequest};
use crate::store::Store;

/// The synced daemon of `draft_tests` with m1 listing `plan.pdf` (part `0.1`), its bytes in
/// the fake Gmail, and a destination directory at mode 0755, as a download directory is.
async fn fixture() -> (DaemonHarness, PathBuf) {
    let h = DaemonHarness::new().await;
    let message = h.message_id("m1");
    Store::open(&h.paths.db_file())
        .unwrap()
        .conn()
        .execute(
            "INSERT INTO attachments (message_id, remote_part_id, filename, mime_type, size)
             VALUES (?1, '0.1', 'plan.pdf', 'application/pdf', 2048)",
            rusqlite::params![message],
        )
        .unwrap();
    h.mock.with(|m| {
        m.attachment_bytes
            .insert(("m1".to_string(), "0.1".to_string()), vec![7u8; 2048]);
    });
    let dest = h.paths.cache_dir.join("quarantined");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
    (h, dest)
}

/// The value of `com.apple.quarantine` on `path`, or `None` when the file carries none.
fn quarantine_of(path: &Path) -> Option<String> {
    let output = std::process::Command::new("/usr/bin/xattr")
        .arg("-p")
        .arg("com.apple.quarantine")
        .arg(path)
        .output()
        .expect("xattr runs");
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// DELTA's quarantine ruling: a saved file and an opened copy are both marked as downloads,
/// with the flags, a time and Rail as the agent.
#[tokio::test]
async fn every_file_rail_writes_is_quarantined_like_a_download() {
    let (h, dest) = fixture().await;
    let saved = h
        .call::<AttachmentSave>(&AttachmentSaveRequest {
            account: "work".into(),
            message_id: "m1".into(),
            part_ids: vec!["0.1".into()],
            dir: dest.display().to_string(),
        })
        .await
        .expect("the save answered");
    assert_eq!(saved.results.len(), 1, "{:?}", saved.results);
    let AttachmentOutcome::Saved { path, .. } = &saved.results[0].outcome else {
        panic!("not saved: {:?}", saved.results[0].outcome);
    };
    let written = PathBuf::from(path);
    let mark = quarantine_of(&written).expect("the saved file is quarantined");
    assert!(mark.starts_with("0083;"), "{mark}");
    assert!(mark.ends_with(";Rail;"), "{mark}");

    let opened = h
        .call::<AttachmentOpen>(&AttachmentOpenRequest {
            account: "work".into(),
            message_id: "m1".into(),
            part_id: "0.1".into(),
        })
        .await
        .expect("the open answered");
    let copy = PathBuf::from(&opened.path);
    let mark = quarantine_of(&copy).expect("the opened copy is quarantined");
    assert!(mark.starts_with("0083;"), "{mark}");
    assert!(mark.ends_with(";Rail;"), "{mark}");
}
