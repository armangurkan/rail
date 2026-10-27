//! `attachment.save` and `attachment.open` through a real daemon over the fake Gmail: the
//! private file a save writes (and the destination it leaves alone), the names it never
//! replaces, an empty part list, the refusals a bad destination and an unknown part get, the
//! private copy an open writes under the cache, and the stale copies the sweeps remove.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use super::client::DaemonClient;
use super::draft_tests::{BOUND, DaemonHarness, WORK};
use super::{Connector, ServeConfig};
use crate::error::ErrorClass;
use crate::ops::op::{AttachmentOpen, AttachmentSave, SyncStatus};
use crate::ops::{
    AttachmentOpenRequest, AttachmentOpenResponse, AttachmentOutcome, AttachmentSaveRequest,
    AttachmentSaveResponse, ErrorCode, OpError, SyncStatusRequest, names,
};
use crate::paths::Paths;
use crate::provider::ProviderError;
use crate::provider::graph::REFERENCE_ATTACHMENT;
use crate::provider::limits::MAX_ATTACHMENTS;
use crate::provider::mock::{Call, MockProvider, envelope};
use crate::store::Store;
use crate::test_support::{daemon_context, mock_factory, start_daemon};

/// Fixture **D**: the synced daemon of `draft_tests`, m1 listing `plan.pdf` (part `0.1`) whose
/// 2048 bytes Gmail has, and a destination directory at mode 0755.
async fn fixture() -> (DaemonHarness, PathBuf) {
    let h = DaemonHarness::new().await;
    list_part(&h, "0.1", "plan.pdf", "application/pdf", 2048);
    bytes_at(&h, "0.1", vec![7u8; 2048]);
    let dest = destination(&h, "saved");
    (h, dest)
}

/// A directory under the cache at mode 0755, as a user's download directory is.
fn destination(h: &DaemonHarness, name: &str) -> PathBuf {
    let dest = h.paths.cache_dir.join(name);
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).unwrap();
    dest
}

/// Tell the store m1 has this attachment, as a body fetch would.
fn list_part(h: &DaemonHarness, part: &str, filename: &str, mime: &str, size: i64) {
    let message = h.message_id("m1");
    Store::open(&h.paths.db_file())
        .unwrap()
        .conn()
        .execute(
            "INSERT INTO attachments (message_id, remote_part_id, filename, mime_type, size)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![message, part, filename, mime, size],
        )
        .unwrap();
}

/// What Gmail answers for m1's part.
fn bytes_at(h: &DaemonHarness, part: &str, bytes: Vec<u8>) {
    h.mock.with(|m| {
        m.attachment_bytes
            .insert(("m1".to_string(), part.to_string()), bytes);
    });
}

fn dir_text(path: &Path) -> String {
    path.display().to_string()
}

async fn save(
    h: &DaemonHarness,
    part_ids: &[&str],
    dir: &Path,
) -> Result<AttachmentSaveResponse, OpError> {
    save_into(h, part_ids, &dir_text(dir)).await
}

async fn save_into(
    h: &DaemonHarness,
    part_ids: &[&str],
    dir: &str,
) -> Result<AttachmentSaveResponse, OpError> {
    let request = AttachmentSaveRequest {
        account: "work".into(),
        message_id: "m1".into(),
        part_ids: part_ids.iter().map(|p| (*p).to_string()).collect(),
        dir: dir.to_string(),
    };
    h.call::<AttachmentSave>(&request).await
}

async fn open(h: &DaemonHarness, part: &str) -> Result<AttachmentOpenResponse, OpError> {
    let request = AttachmentOpenRequest {
        account: "work".into(),
        message_id: "m1".into(),
        part_id: part.to_string(),
    };
    h.call::<AttachmentOpen>(&request).await
}

/// The path of the one result, which must be `Saved`.
fn written(response: &AttachmentSaveResponse) -> PathBuf {
    assert_eq!(response.results.len(), 1, "{:?}", response.results);
    saved_path(&response.results[0].outcome)
}

fn saved_path(outcome: &AttachmentOutcome) -> PathBuf {
    let AttachmentOutcome::Saved { path, .. } = outcome else {
        panic!("not saved: {outcome:?}");
    };
    PathBuf::from(path)
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn attachment_calls(h: &DaemonHarness) -> usize {
    h.mock
        .calls()
        .iter()
        .filter(|c| **c == Call::Attachment)
        .count()
}

/// The files directly under `dir`, sorted; an absent directory holds none.
fn entries(dir: &Path) -> Vec<String> {
    let Ok(read) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = read
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// Nothing is in `dir`, and the names it holds when there are any.
fn nothing_written(dir: &Path) {
    assert_eq!(entries(dir), Vec::<String>::new(), "nothing was written");
}

/// Set `path`'s modification time `seconds` into the past.
fn age(path: &Path, seconds: u64) {
    let when = SystemTime::now() - Duration::from_secs(seconds);
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

/// A copy under `attachments_dir()`, in its own token directory, `seconds` old.
fn stale_copy(paths: &Paths, token: &str, seconds: u64) -> PathBuf {
    let dir = paths.attachments_dir().join(token);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.pdf");
    std::fs::write(&path, b"old").unwrap();
    age(&path, seconds);
    path
}

/// AC-15: a save writes one 0600 file with the provider's bytes and leaves the destination's
/// own mode alone.
#[tokio::test]
async fn save_writes_a_private_file_and_leaves_the_directory_alone() {
    let (h, dest) = fixture().await;
    let response = save(&h, &["0.1"], &dest).await.unwrap();
    assert_eq!(response.results[0].part_id, "0.1");
    let AttachmentOutcome::Saved { path, size } = &response.results[0].outcome else {
        panic!("{:?}", response.results[0].outcome);
    };
    assert_eq!(Path::new(path), dest.join("plan.pdf"));
    assert_eq!(*size, 2048);
    assert_eq!(std::fs::read(path).unwrap(), vec![7u8; 2048]);
    assert_eq!(mode(Path::new(path)), 0o600);
    assert_eq!(mode(&dest), 0o755, "the destination is untouched");
}

/// AC-16: a second save of the same name writes a new file beside the first.
#[tokio::test]
async fn a_second_save_never_overwrites_the_first() {
    let (h, dest) = fixture().await;
    let first = written(&save(&h, &["0.1"], &dest).await.unwrap());
    std::fs::write(&first, b"mine").unwrap();
    let second = written(&save(&h, &["0.1"], &dest).await.unwrap());
    assert_eq!(second, dest.join("2-plan.pdf"));
    assert_eq!(std::fs::read(&first).unwrap(), b"mine", "the first is kept");
    assert!(second.is_file());
}

/// AC-17: the name a save avoids is matched whatever its case.
#[tokio::test]
async fn the_dedup_ignores_case() {
    let (h, dest) = fixture().await;
    std::fs::write(dest.join("PLAN.PDF"), b"mine").unwrap();
    let path = written(&save(&h, &["0.1"], &dest).await.unwrap());
    assert_eq!(path, dest.join("2-plan.pdf"));
    assert_eq!(
        std::fs::read(dest.join("PLAN.PDF")).unwrap(),
        b"mine",
        "the existing file is kept"
    );
}

/// AC-18: a symlink at the target name is never written through.
#[tokio::test]
async fn a_symlink_at_the_target_name_is_never_written_through() {
    let (h, dest) = fixture().await;
    let outside = h.paths.cache_dir.join("outside.txt");
    std::fs::write(&outside, b"keep").unwrap();
    std::os::unix::fs::symlink(&outside, dest.join("plan.pdf")).unwrap();
    let path = written(&save(&h, &["0.1"], &dest).await.unwrap());
    assert_eq!(path, dest.join("2-plan.pdf"));
    assert_eq!(
        std::fs::read(&outside).unwrap(),
        b"keep",
        "the symlink's target is untouched"
    );
}

/// AC-16b: a name that would hide the file loses its leading dots.
#[tokio::test]
async fn a_hidden_name_is_made_visible() {
    let (h, dest) = fixture().await;
    list_part(&h, "0.2", ".zshenv", "text/plain", 4);
    bytes_at(&h, "0.2", b"shell".to_vec());
    list_part(&h, "0.3", "...", "text/plain", 4);
    bytes_at(&h, "0.3", b"dots".to_vec());
    let path = written(&save(&h, &["0.2"], &dest).await.unwrap());
    assert_eq!(path, dest.join("zshenv"));
    let path = written(&save(&h, &["0.3"], &dest).await.unwrap());
    assert_eq!(path, dest.join("attachment"));
}

/// FR-11, as AC-16b's `.zshenv`: a name that only *looks* visible is made visible too.
/// `sanitize_filename` keeps the space between a leading dot and the rest, and
/// `draft_files::file_name` sanitizes every candidate again — trimming that space — so stripping
/// the dots alone still wrote `.zshenv`.
#[tokio::test]
async fn a_name_that_only_looks_visible_is_still_made_visible() {
    let (h, dest) = fixture().await;
    let names = [
        ". .zshenv",
        ".\t.npmrc",
        "..\u{a0}.bashrc",
        " .x",
        ". . .zshenv",
        ".\u{2028}.y",
    ];
    for (n, name) in names.iter().enumerate() {
        let part = format!("1.{n}");
        list_part(&h, &part, name, "text/plain", 5);
        bytes_at(&h, &part, b"shell".to_vec());
        let path = written(&save(&h, &[part.as_str()], &dest).await.unwrap());
        assert!(path.is_file(), "{name:?} wrote nothing");
    }
    // Read back from the directory itself, not from the answer: the name on disk is the one
    // that would be hidden.
    let on_disk = entries(&dest);
    assert_eq!(on_disk.len(), names.len(), "{on_disk:?}");
    for name in &on_disk {
        assert!(
            !name.starts_with('.') && !name.starts_with('-'),
            "{name:?} is not visible"
        );
    }
}

/// FR-11: a leading dash goes the same way as a leading dot. A sender's
/// `--checkpoint-action=exec=sh run.sh` must not land in the download directory as a name a
/// later glob hands to a command as an option.
#[tokio::test]
async fn an_option_shaped_name_loses_its_dashes() {
    let (h, dest) = fixture().await;
    let names = [
        ("--checkpoint=1", "checkpoint=1"),
        ("-rf", "rf"),
        (" - . -", "attachment"),
    ];
    for (n, (name, written_as)) in names.into_iter().enumerate() {
        let part = format!("2.{n}");
        list_part(&h, &part, name, "text/plain", 2);
        bytes_at(&h, &part, b"hi".to_vec());
        let path = written(&save(&h, &[part.as_str()], &dest).await.unwrap());
        assert_eq!(path, dest.join(written_as), "{name:?}");
        assert!(path.is_file(), "{name:?} wrote nothing");
    }
}

/// §7's part-id limit, through FR-4's `s` and FR-5's `Enter`: Gmail stores `""` as the part
/// id of a single-part message whose top part is the
/// file itself (`gmail/body.rs`'s `part_id_ok` keeps it, and `find_part` matches the payload
/// for it), so `s`, `Enter` and `o` have to reach that part: an empty **part** id is one both
/// operations take.
#[tokio::test]
async fn a_part_whose_id_is_empty_is_saved_and_opened() {
    let (h, dest) = fixture().await;
    list_part(&h, "", "photo.png", "image/png", 3);
    bytes_at(&h, "", b"png".to_vec());
    let response = save(&h, &[""], &dest).await.unwrap();
    assert_eq!(response.results[0].part_id, "");
    let path = written(&response);
    assert_eq!(path, dest.join("photo.png"));
    assert_eq!(std::fs::read(&path).unwrap(), b"png");
    let copy = PathBuf::from(open(&h, "").await.unwrap().path);
    assert!(copy.ends_with("photo.png"), "{}", copy.display());
    assert_eq!(std::fs::read(&copy).unwrap(), b"png");
}

/// AC-19: an empty part list saves every attachment that message has, in the store's order:
/// one result per part the store lists, however many that is. Nothing here caps them — the
/// sync's own `MAX_ATTACHMENTS` is what bounds what the store can list for one message.
#[tokio::test]
async fn an_empty_part_list_saves_every_attachment_of_that_message() {
    let (h, dest) = fixture().await;
    list_part(&h, "0.2", "logo.png", "image/png", 3);
    bytes_at(&h, "0.2", b"png".to_vec());
    list_part(&h, "0.3", "notes.txt", "text/plain", 5);
    bytes_at(&h, "0.3", b"notes".to_vec());
    let before = attachment_calls(&h);
    let response = save(&h, &[], &dest).await.unwrap();
    let parts: Vec<&str> = response
        .results
        .iter()
        .map(|r| r.part_id.as_str())
        .collect();
    assert_eq!(parts, ["0.1", "0.2", "0.3"], "the store's order");
    for result in &response.results {
        assert_eq!(mode(&saved_path(&result.outcome)), 0o600);
    }
    assert_eq!(entries(&dest).len(), 3);
    assert_eq!(attachment_calls(&h) - before, 3);

    let (h, dest) = fixture().await;
    for n in 2..=MAX_ATTACHMENTS {
        let part = format!("0.{n}");
        list_part(&h, &part, &format!("part-{n}.txt"), "text/plain", 2);
        bytes_at(&h, &part, b"hi".to_vec());
    }
    let response = save(&h, &[], &dest).await.unwrap();
    assert_eq!(response.results.len(), MAX_ATTACHMENTS);
}

/// AC-19b: a part named twice is saved once.
#[tokio::test]
async fn a_duplicate_part_id_is_saved_once() {
    let (h, dest) = fixture().await;
    let before = attachment_calls(&h);
    let response = save(&h, &["0.1", "0.1"], &dest).await.unwrap();
    assert_eq!(response.results.len(), 1, "{:?}", response.results);
    assert_eq!(attachment_calls(&h) - before, 1);
    assert_eq!(entries(&dest), ["plan.pdf"]);
}

/// AC-20: a part the store does not list is `Missing`, with nothing asked of the provider.
#[tokio::test]
async fn an_unknown_part_is_missing_without_a_provider_call() {
    let (h, dest) = fixture().await;
    let before = attachment_calls(&h);
    let response = save(&h, &["9.9"], &dest).await.unwrap();
    assert_eq!(response.results.len(), 1);
    assert!(
        matches!(response.results[0].outcome, AttachmentOutcome::Missing),
        "{:?}",
        response.results[0].outcome
    );
    assert_eq!(attachment_calls(&h), before, "Gmail was not asked");
    nothing_written(&dest);
}

/// AC-21: a provider that no longer has the part is `Missing`; one with network trouble is a
/// transient failure that leaves no file.
#[tokio::test]
async fn provider_failures_map_to_missing_and_failed() {
    let (h, dest) = fixture().await;
    list_part(&h, "0.2", "logo.png", "image/png", 3);
    let response = save(&h, &["0.2"], &dest).await.unwrap();
    assert!(
        matches!(response.results[0].outcome, AttachmentOutcome::Missing),
        "{:?}",
        response.results[0].outcome
    );
    nothing_written(&dest);

    h.mock
        .fail_next(Call::Attachment, ProviderError::transient("Gmail is busy"));
    let response = save(&h, &["0.1"], &dest).await.unwrap();
    let AttachmentOutcome::Failed(error) = &response.results[0].outcome else {
        panic!("{:?}", response.results[0].outcome);
    };
    assert_eq!(error.class, ErrorClass::Transient);
    assert!(entries(&dest).is_empty(), "{:?}", entries(&dest));
}

/// FR-13 (one part fetched at a time): a network failure stops the save. Every part after the first
/// such failure is given that same failure without a fetch of its own, so a save of a many-part
/// message cannot hold `Saving…` up for a retry run per part while the network is down.
#[tokio::test]
async fn a_network_failure_stops_the_fetches_that_would_follow() {
    let (h, dest) = fixture().await;
    list_part(&h, "0.2", "logo.png", "image/png", 3);
    bytes_at(&h, "0.2", b"png".to_vec());
    list_part(&h, "0.3", "notes.txt", "text/plain", 5);
    bytes_at(&h, "0.3", b"notes".to_vec());
    for _ in 0..3 {
        h.mock.fail_next(
            Call::Attachment,
            ProviderError::transient("the network went"),
        );
    }
    let before = attachment_calls(&h);
    let response = save(&h, &[], &dest).await.unwrap();
    assert_eq!(response.results.len(), 3, "{:?}", response.results);
    for result in &response.results {
        let AttachmentOutcome::Failed(error) = &result.outcome else {
            panic!("{}: {:?}", result.part_id, result.outcome);
        };
        assert_eq!(error.code, ErrorCode::Network, "{}", result.part_id);
        assert_eq!(error.class, ErrorClass::Transient, "{}", result.part_id);
    }
    assert_eq!(
        attachment_calls(&h) - before,
        1,
        "only the first part was fetched"
    );
    nothing_written(&dest);
}

/// AC-22: a Microsoft 365 reference attachment's permanent message passes through verbatim.
#[tokio::test]
async fn a_reference_attachment_says_where_to_reach_it() {
    let (h, dest) = fixture().await;
    h.mock.fail_next(
        Call::Attachment,
        ProviderError::permanent(REFERENCE_ATTACHMENT),
    );
    let response = save(&h, &["0.1"], &dest).await.unwrap();
    let AttachmentOutcome::Failed(error) = &response.results[0].outcome else {
        panic!("{:?}", response.results[0].outcome);
    };
    assert_eq!(error.class, ErrorClass::Permanent);
    assert_eq!(error.message, REFERENCE_ATTACHMENT);
}

/// AC-21b: an account paused for a login fails the save before any store or provider call.
#[tokio::test]
async fn save_on_an_account_needing_login_fails_before_any_fetch() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let mock = MockProvider::new();
    mock.seed(envelope("m1", "t1", crate::auth::unix_now() * 1000));
    mock.fail_next(Call::Folders, ProviderError::NeedsAuth { detail: None });
    let (ctx, _) = daemon_context(&paths);
    start_daemon(
        ctx.with_provider_factory(mock_factory(&mock)),
        ServeConfig::default(),
    )
    .await;
    let client: DaemonClient = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    tokio::time::timeout(BOUND, async {
        loop {
            let request = SyncStatusRequest {
                account: Some("work".into()),
            };
            if let Ok(status) = client.call::<SyncStatus>(&request).await
                && status.accounts[0].needs_auth
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the account was paused for a login");
    let dest = paths.cache_dir.join("saved");
    std::fs::create_dir_all(&dest).unwrap();
    let request = AttachmentSaveRequest {
        account: "work".into(),
        message_id: "m1".into(),
        part_ids: Vec::new(),
        dir: dir_text(&dest),
    };
    let error = client
        .call::<AttachmentSave>(&request)
        .await
        .expect_err("a paused account cannot save");
    assert_eq!(error.code, ErrorCode::Auth);
    assert_eq!(error.class, ErrorClass::Permanent);
    assert!(
        error
            .hint
            .as_deref()
            .is_some_and(|hint| hint.contains("rail account login work")),
        "{error}"
    );
    nothing_written(&dest);
    assert!(
        !mock.calls().contains(&Call::Attachment),
        "Gmail was not asked"
    );
}

/// AC-23: a destination that is not absolute is refused without a provider call, and without
/// naming a file.
#[tokio::test]
async fn a_relative_destination_is_refused() {
    let (h, _dest) = fixture().await;
    for dir in ["relative/path", ""] {
        let before = attachment_calls(&h);
        let error = save_into(&h, &["0.1"], dir)
            .await
            .expect_err("a relative destination");
        assert_eq!(error.code, ErrorCode::InvalidInput, "{dir:?}");
        assert_eq!(error.class, ErrorClass::Permanent, "{dir:?}");
        assert!(error.message.contains("dir"), "{error}");
        assert!(!error.to_string().contains("plan.pdf"), "{error}");
        assert_eq!(attachment_calls(&h), before, "{dir:?}");
    }
}

/// FR-12: Rail's own attachment cache is not a destination. Every copy under it is swept a
/// day after it was written, so a save into it — or into a directory under it — would lose the
/// file; the refusal is permanent, names no file, and nothing is fetched or written.
#[tokio::test]
async fn a_save_into_rail_s_own_cache_is_refused() {
    let (h, _dest) = fixture().await;
    let inside = h.paths.attachments_dir().join("mine");
    for dir in [h.paths.attachments_dir(), inside.clone()] {
        let before = attachment_calls(&h);
        let error = save(&h, &["0.1"], &dir)
            .await
            .expect_err("Rail's own cache");
        assert_eq!(error.code, ErrorCode::InvalidInput, "{}", dir.display());
        assert_eq!(error.class, ErrorClass::Permanent, "{}", dir.display());
        assert!(error.message.contains("dir"), "{error}");
        assert!(!error.to_string().contains("plan.pdf"), "{error}");
        assert_eq!(attachment_calls(&h), before, "Gmail was not asked");
    }
    assert!(!inside.exists(), "the destination was created anyway");
    nothing_written(&h.paths.attachments_dir());
}

/// AC-24: a destination that does not exist yet is created at the process umask, not at 0700.
#[tokio::test]
async fn a_missing_destination_is_created_at_the_umask() {
    let (h, _dest) = fixture().await;
    let control = h.paths.cache_dir.join("control");
    std::fs::create_dir(&control).unwrap();
    let fresh = h.paths.cache_dir.join("fresh");
    let path = written(&save(&h, &["0.1"], &fresh).await.unwrap());
    assert_eq!(path, fresh.join("plan.pdf"));
    assert_eq!(mode(&path), 0o600);
    assert_eq!(
        mode(&fresh),
        mode(&control),
        "the same umask made both, so nothing chmodded it"
    );
}

/// AC-24b: a destination that cannot be written is a permanent refusal.
#[tokio::test]
async fn an_unwritable_destination_is_a_permanent_refusal() {
    let (h, _dest) = fixture().await;
    let file = h.paths.cache_dir.join("not-a-directory");
    std::fs::write(&file, b"x").unwrap();
    let locked = destination(&h, "locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    for dir in [&file, &locked] {
        let before = attachment_calls(&h);
        let error = save(&h, &["0.1"], dir)
            .await
            .expect_err("an unwritable destination");
        assert_eq!(error.code, ErrorCode::InvalidInput, "{}", dir.display());
        assert_eq!(error.class, ErrorClass::Permanent, "{}", dir.display());
        assert_eq!(error.message, "dir: cannot be written");
        assert_eq!(attachment_calls(&h), before);
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
}

/// AC-25: the copy an open writes is named from the stored type, not from the sender's name.
#[tokio::test]
async fn open_names_the_cache_file_from_the_type_not_the_sender_s_name() {
    let (h, _dest) = fixture().await;
    list_part(&h, "0.4", "invoice.html", "image/png", 3);
    bytes_at(&h, "0.4", b"png".to_vec());
    let response = open(&h, "0.4").await.unwrap();
    let path = PathBuf::from(&response.path);
    assert!(
        path.ends_with("invoice.png"),
        "the extension comes from the type: {}",
        path.display()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"png");
    assert_eq!(mode(&path), 0o600);
    let token = path.parent().expect("a token directory");
    assert_eq!(mode(token), 0o700);
    assert_eq!(
        token.parent().expect("the attachments directory"),
        h.paths.attachments_dir()
    );
}

/// FR-14: the copy's own name stays inside the file-name limit. Its stem comes from a name
/// `sanitize_filename` has already capped at 255 bytes, so adding the type's extension pushed
/// the whole name past what a file system takes, and every open of such a part failed.
#[tokio::test]
async fn an_open_copy_keeps_its_name_within_the_file_name_limit() {
    let (h, _dest) = fixture().await;
    let long = "a".repeat(255);
    list_part(&h, "0.7", &long, "application/pdf", 3);
    bytes_at(&h, "0.7", b"pdf".to_vec());
    let response = open(&h, "0.7").await.unwrap();
    let path = PathBuf::from(&response.path);
    let name = path.file_name().expect("a file name").to_string_lossy();
    assert!(name.len() <= 255, "{} bytes: {name}", name.len());
    assert!(name.ends_with(".pdf"), "{name}");
    assert_eq!(std::fs::read(&path).unwrap(), b"pdf");
    assert_eq!(mode(&path), 0o600);
}

/// AC-26: a type Rail does not open is refused before any provider call.
#[tokio::test]
async fn open_refuses_a_type_rail_does_not_open() {
    let (h, _dest) = fixture().await;
    list_part(&h, "0.5", "page.html", "text/html", 3);
    bytes_at(&h, "0.5", b"<p>".to_vec());
    let before = attachment_calls(&h);
    let error = open(&h, "0.5").await.expect_err("an unopenable type");
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert_eq!(error.class, ErrorClass::Permanent);
    assert_eq!(attachment_calls(&h), before, "Gmail was not asked");
    nothing_written(&h.paths.attachments_dir());
}

/// AC-26b: an open of a part the store does not list names no part and asks nothing.
#[tokio::test]
async fn open_refuses_an_unlisted_part_without_a_provider_call() {
    let (h, _dest) = fixture().await;
    let before = attachment_calls(&h);
    let error = open(&h, "9.9").await.expect_err("an unlisted part");
    assert_eq!(error.code, ErrorCode::NotFound);
    assert_eq!(error.class, ErrorClass::Permanent);
    assert!(!error.to_string().contains("9.9"), "{error}");
    assert_eq!(attachment_calls(&h), before);
    nothing_written(&h.paths.attachments_dir());
}

/// AC-27: the sweep at daemon start removes the copies older than a day, and only those.
#[tokio::test]
async fn the_start_sweep_removes_only_stale_open_files() {
    let (h, _dest) = fixture().await;
    let stale = stale_copy(&h.paths, "old", 25 * 3600);
    let fresh = stale_copy(&h.paths, "new", 3600);
    let h = h.restart().await;
    h.until("the stale copy swept", |h| {
        !h.paths.attachments_dir().join("old").exists()
    })
    .await;
    assert!(!stale.exists(), "the old file went");
    assert!(fresh.is_file(), "the fresh file stayed");
}

/// AC-27b: every open sweeps the stale copies first.
#[tokio::test]
async fn an_open_sweeps_stale_copies_first() {
    let (h, _dest) = fixture().await;
    list_part(&h, "0.4", "invoice.pdf", "application/pdf", 3);
    bytes_at(&h, "0.4", b"pdf".to_vec());
    let stale = stale_copy(&h.paths, "old", 25 * 3600);
    let fresh = stale_copy(&h.paths, "new", 3600);
    let response = open(&h, "0.4").await.unwrap();
    assert!(!stale.exists(), "the old file went");
    assert!(
        !h.paths.attachments_dir().join("old").exists(),
        "its empty directory went too"
    );
    assert!(fresh.is_file(), "the fresh file stayed");
    assert!(Path::new(&response.path).is_file(), "the new copy is there");
}

/// AC-29b: both handlers are registered.
#[tokio::test]
async fn the_two_handlers_are_registered() {
    let served = super::handlers::registry().names();
    assert!(served.contains(&names::ATTACHMENT_SAVE), "{served:?}");
    assert!(served.contains(&names::ATTACHMENT_OPEN), "{served:?}");
}
