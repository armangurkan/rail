//! `saved_search.put` and `saved_search.remove`: the wire names, and the two handlers through
//! a real daemon on a temporary `config.toml`. AC-20, AC-20b, AC-20c, AC-21, AC-28.

use std::sync::Arc;

use super::client::DaemonClient;
use super::{Connector, ServeConfig};
use crate::config::{MAX_SAVED_SEARCHES, SavedSearch};
use crate::error::ErrorClass;
use crate::ops::op::{SavedSearchPut, SavedSearchRemove};
use crate::ops::{
    ErrorCode, SavedSearchListResponse, SavedSearchPutRequest, SavedSearchRemoveRequest, names,
};
use crate::paths::Paths;
use crate::test_support::{daemon_context, start_daemon};

/// `# mine`, so every write can be checked for keeping it, and one account.
const MINE: &str = "# mine\n[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";

/// A real daemon over a real socket on a temporary config, the `manage_tests` shape. The
/// config directory is created 0700, so AC-20 can assert the write left it alone.
async fn daemon(tmp: &tempfile::TempDir, config: &str) -> (Paths, DaemonClient) {
    let paths = Paths::all_in(tmp.path());
    crate::paths::create_private_dir(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), config).unwrap();
    let (ctx, _) = daemon_context(&paths);
    let _daemon = start_daemon(ctx, ServeConfig::default()).await;
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    (paths, client)
}

fn put(name: &str, query: &str, replaces: Option<&str>) -> SavedSearchPutRequest {
    SavedSearchPutRequest {
        name: name.into(),
        query: query.into(),
        replaces: replaces.map(Into::into),
    }
}

fn remove(name: &str) -> SavedSearchRemoveRequest {
    SavedSearchRemoveRequest { name: name.into() }
}

fn one(name: &str, query: &str) -> SavedSearchListResponse {
    SavedSearchListResponse {
        searches: vec![SavedSearch {
            name: name.into(),
            query: query.into(),
        }],
    }
}

#[cfg(unix)]
fn mode(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// AC-28: asserted **relatively**, so it passes on this branch (where `ALL` is `[&str; 26]`
/// and the attachments consts do not exist) and still passes once that branch merges ahead of
/// it with its own pair. The retry clause asserts the public `retry_safe`, not `RETRY_SAFE`,
/// which is a private const of `connect.rs`; the array itself is covered by
/// `connect_tests.rs`'s `unsafe_op` list.
#[test]
fn the_saved_search_names_are_registered_and_never_retried() {
    let at = |name: &str| {
        names::ALL
            .iter()
            .position(|n| *n == name)
            .unwrap_or_else(|| panic!("{name} is not in names::ALL"))
    };
    let put = at(names::SAVED_SEARCH_PUT);
    let remove = at(names::SAVED_SEARCH_REMOVE);
    assert_eq!(put + 1, remove, "the two names are adjacent, put first");
    assert!(put > at(names::DRAFT_IMPORT), "appended after draft.import");

    // Neither carries a client token, so a resent `put` would be refused for a clashing name
    // and a resent `remove` would fail `NotFound`.
    assert!(!super::connect::retry_safe(names::SAVED_SEARCH_PUT));
    assert!(!super::connect::retry_safe(names::SAVED_SEARCH_REMOVE));

    let served = super::handlers::registry().names();
    assert!(served.contains(&names::SAVED_SEARCH_PUT), "{served:?}");
    assert!(served.contains(&names::SAVED_SEARCH_REMOVE), "{served:?}");
}

/// AC-20 (FR-20, §8): the comment, the other table and the file's mode survive a save, a
/// rename and a removal, and the write leaves the config directory's own mode alone — it must
/// neither tighten nor loosen a directory the user already has (D-9).
#[cfg(unix)]
#[tokio::test]
async fn put_rename_and_remove_keep_the_file_and_its_comments() {
    let tmp = tempfile::tempdir().unwrap();
    let (paths, client) = daemon(&tmp, MINE).await;
    let file = paths.config_file();
    assert_eq!(mode(&paths.config_dir), 0o700, "the test created it 0700");

    let saved = client
        .call::<SavedSearchPut>(&put("dana", "from:dana", None))
        .await
        .unwrap();
    assert_eq!(saved, one("dana", "from:dana"));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("# mine"), "{text}");
    assert!(text.contains("[[accounts]]"), "{text}");
    assert!(text.contains("[[saved_searches]]"), "{text}");
    assert!(text.contains("name = \"dana\""), "{text}");
    assert!(text.contains("query = \"from:dana\""), "{text}");
    assert_eq!(mode(&file), 0o600);
    assert_eq!(mode(&paths.config_dir), 0o700, "the directory is untouched");

    let renamed = client
        .call::<SavedSearchPut>(&put("dana2", "from:dana", Some("dana")))
        .await
        .unwrap();
    assert_eq!(renamed, one("dana2", "from:dana"));
    let text = std::fs::read_to_string(&file).unwrap();
    assert_eq!(text.matches("[[saved_searches]]").count(), 1, "{text}");
    assert!(text.contains("name = \"dana2\""), "{text}");

    let gone = client
        .call::<SavedSearchRemove>(&remove("dana2"))
        .await
        .unwrap();
    assert!(gone.searches.is_empty(), "{gone:?}");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains("[[saved_searches]]"), "{text}");
    assert!(text.contains("# mine"), "{text}");
    assert_eq!(mode(&paths.config_dir), 0o700);
}

/// AC-20b (§8 Symlinks, D-9): a dotfile-managed config is written where it points, so the link
/// survives, and the directory it points into is not tightened.
#[cfg(unix)]
#[tokio::test]
async fn a_symlinked_config_is_written_where_it_points() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempfile::tempdir().unwrap();
    let paths = Paths::all_in(tmp.path());
    crate::paths::create_private_dir(&paths.config_dir).unwrap();
    let other = paths.config_dir.join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::set_permissions(&other, std::fs::Permissions::from_mode(0o755)).unwrap();
    let target = other.join("config.toml");
    std::fs::write(&target, MINE).unwrap();
    std::os::unix::fs::symlink(&target, paths.config_file()).unwrap();

    let (ctx, _) = daemon_context(&paths);
    let _daemon = start_daemon(ctx, ServeConfig::default()).await;
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    let saved = client
        .call::<SavedSearchPut>(&put("dana", "from:dana", None))
        .await
        .unwrap();
    assert_eq!(saved, one("dana", "from:dana"));

    assert!(
        std::fs::symlink_metadata(paths.config_file())
            .unwrap()
            .file_type()
            .is_symlink(),
        "the link survives"
    );
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(text.contains("[[saved_searches]]"), "{text}");
    assert!(text.contains("# mine"), "{text}");
    assert_eq!(mode(&target), 0o600);
    assert_eq!(mode(&other), 0o755, "the user's directory is not tightened");
}

/// AC-21 (§8's error table): every refusal names its own code and changes nothing. None may
/// arrive as `ErrorCode::Config`, which the bare `handlers/remove.rs` idiom would produce for
/// all of them, since a `ConfigError::Invalid` maps to `Config` with the reason in `detail`.
#[tokio::test]
async fn refusals_change_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (paths, client) = daemon(&tmp, MINE).await;
    let file = paths.config_file();
    client
        .call::<SavedSearchPut>(&put("dana", "from:dana", None))
        .await
        .unwrap();
    let before = std::fs::read_to_string(&file).unwrap();

    let err = client
        .call::<SavedSearchRemove>(&remove("nope"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
    assert_eq!(err.class, ErrorClass::Permanent);
    assert_eq!(err.message, "There is no saved search named \"nope\".");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);

    let err = client
        .call::<SavedSearchPut>(&put("DANA", "is:unread", None))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Duplicate, "{err:?}");
    assert_eq!(err.class, ErrorClass::Permanent);
    assert_eq!(err.message, "A saved search named \"DANA\" already exists.");

    let err = client
        .call::<SavedSearchPut>(&put("two words", "is:unread", None))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput, "{err:?}");
    assert_eq!(err.class, ErrorClass::Permanent);

    let err = client
        .call::<SavedSearchPut>(&put("gone", "is:unread", Some("absent")))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound, "{err:?}");
    assert_eq!(err.message, "There is no saved search named \"absent\".");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), before);

    // A rename to a case-changed name is FR-4's carve-out, so it succeeds.
    let renamed = client
        .call::<SavedSearchPut>(&put("Dana", "from:dana", Some("dana")))
        .await
        .unwrap();
    assert_eq!(renamed, one("Dana", "from:dana"));
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("name = \"Dana\""),
    );

    // Fill the cap, then one more.
    for i in 1..MAX_SAVED_SEARCHES {
        client
            .call::<SavedSearchPut>(&put(&format!("s{i}"), "is:unread", None))
            .await
            .unwrap();
    }
    let full = std::fs::read_to_string(&file).unwrap();
    let err = client
        .call::<SavedSearchPut>(&put("over", "is:unread", None))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput, "{err:?}");
    assert_eq!(err.class, ErrorClass::Permanent);
    assert_eq!(err.message, "Rail keeps at most 20 saved searches.");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), full);
}

/// AC-20c (FR-23): both writers of `config.toml` hold one cross-process lock for the whole
/// read-edit-rename, so a save and a `[general]` toggle inside one write window can no longer
/// lose a change. The in-process half is precedented: `hold_daemon_lock` in `connect_tests.rs`
/// takes `fs4`'s lock on its own descriptor and three tests rely on the same process then
/// seeing the file as locked.
#[cfg(unix)]
#[tokio::test]
async fn a_second_config_write_waits_for_the_lock_and_keeps_both_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let (paths, client) = daemon(&tmp, MINE).await;
    let file = paths.config_file();
    let lock = crate::config::lock::lock_config(&paths.config_file()).unwrap();
    let lock_file = paths.config_dir.join("config.lock");
    assert_eq!(mode(&lock_file), 0o600);
    let before = std::fs::read_to_string(&file).unwrap();

    let mut handle = tokio::spawn(async move {
        client
            .call::<SavedSearchPut>(&put("dana", "from:dana", None))
            .await
    });
    // The handler's closure blocks on a `spawn_blocking` thread, so this runtime stays free.
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(200), &mut handle)
            .await
            .is_err(),
        "the write waits for the lock"
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        before,
        "nothing written while the lock is held"
    );

    // The other writer's change, made under the lock, then released.
    let toggled = before.replace(
        "[general]\nsecrets = \"file\"\n",
        "[general]\nsecrets = \"file\"\nsaved_search_counts = false\n",
    );
    assert_ne!(toggled, before, "the key went into [general]");
    std::fs::write(&file, &toggled).unwrap();
    drop(lock);

    let saved = handle.await.unwrap().unwrap();
    assert_eq!(saved, one("dana", "from:dana"));
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(
        text.contains("saved_search_counts = false"),
        "the blocked write re-read the text it waited for: {text}"
    );
    assert!(text.contains("name = \"dana\""), "{text}");
    no_temporary_config_left(&paths.config_dir);
}

/// No `config.toml.<…>.tmp` sibling was left behind in `dir`. Asserted on the listing, not on
/// one name: `paths::replace_private_atomic` calls its sibling `config.toml.<pid>-<n>.tmp`
/// (`paths.rs`'s `temp_sibling`), so `config.toml.tmp` is absent whether or not the write
/// cleaned up — the listing is the only thing that can tell. Finding `config.toml` in it is
/// what says the directory was read at all.
fn no_temporary_config_left(dir: &std::path::Path) {
    let names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert!(
        names.iter().any(|name| name == "config.toml"),
        "the listing is of the real directory: {names:?}"
    );
    let left: Vec<&String> = names
        .iter()
        .filter(|name| name.starts_with("config.toml.") && name.ends_with(".tmp"))
        .collect();
    assert!(
        left.is_empty(),
        "a temporary file was left behind: {left:?}"
    );
}
