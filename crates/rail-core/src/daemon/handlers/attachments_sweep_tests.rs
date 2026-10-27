//! The sweep's own two loops. A copy's directory can go between the moment the sweep lists it
//! and the moment it reads it — the reader removes the copies it opened (`tui::open`), and the
//! sweep before every open runs beside the one at start — and a directory the sweep cannot read
//! at all must not stop it from sweeping the rest. Neither can be raced into a real sweep from
//! a test, so the loops are driven directly, beside the handler they belong to.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use super::attachments::{sweep, sweep_token};
use crate::paths::Paths;

/// A token directory under `root` holding one file `seconds` old; that file's path.
fn copy_in(root: &Path, token: &str, seconds: u64) -> PathBuf {
    let dir = root.join(token);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.pdf");
    fs::write(&path, b"old").unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(seconds))
        .unwrap();
    path
}

/// A sweep reports what it could not do, but only after it has swept the rest: a token
/// directory that has gone since it was listed is no failure at all, and one it cannot read is
/// not the end of the sweep.
#[test]
fn a_sweep_keeps_going_past_an_entry_it_cannot_read() {
    let tmp = tempfile::tempdir().unwrap();
    let cutoff = SystemTime::now() - Duration::from_secs(3600);
    assert_eq!(
        sweep_token(&tmp.path().join("gone"), cutoff).unwrap(),
        0,
        "a directory that is no longer there has nothing stale in it"
    );
    // Which of the two the listing hands over first is the file system's choice, so the
    // unreadable one is tried under both names: one of the two orders has it first.
    for (unreadable, holding) in [("a", "b"), ("b", "a")] {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(home.path());
        let root = paths.attachments_dir();
        let locked = root.join(unreadable);
        fs::create_dir_all(&locked).unwrap();
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let stale = copy_in(&root, holding, 25 * 3600);
        let error = sweep(&paths, SystemTime::now()).expect_err("the directory it cannot read");
        assert_eq!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied,
            "{error}"
        );
        assert!(
            !stale.exists(),
            "{unreadable:?} stopped the sweep before {holding:?}"
        );
        // Back to readable, or the temporary directory cannot be removed.
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    }
}
