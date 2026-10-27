//! Where Rail keeps its files. XDG on Linux, platform dirs elsewhere,
//! `RAIL_*_DIR` environment overrides everywhere.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use directories::ProjectDirs;

use crate::error::ErrorClass;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    /// Holds the daemon's socket and start lock. Owner-only.
    pub runtime_dir: PathBuf,
}

#[derive(Debug, Default, Clone)]
pub struct PathOverrides {
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
    pub runtime_dir: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "cannot determine a home directory; set RAIL_CONFIG_DIR, RAIL_DATA_DIR, RAIL_CACHE_DIR, RAIL_STATE_DIR and RAIL_RUNTIME_DIR"
)]
pub struct NoHomeDir;

impl NoHomeDir {
    /// Retrying cannot conjure a home directory.
    pub fn class(&self) -> ErrorClass {
        ErrorClass::Permanent
    }
}

fn need(dirs: &Option<ProjectDirs>) -> Result<&ProjectDirs, NoHomeDir> {
    dirs.as_ref().ok_or(NoHomeDir)
}

impl Paths {
    /// Resolve from `RAIL_*_DIR` environment variables, falling back to platform dirs.
    /// An empty variable counts as unset, so `RAIL_DATA_DIR=""` never means the current directory.
    /// A relative override is made absolute against the current directory, so the daemon (which
    /// runs elsewhere) and every client agree on what it names.
    pub fn from_env() -> Result<Self, NoHomeDir> {
        let cwd = std::env::current_dir().unwrap_or_default();
        Self::from_lookup(|key| std::env::var_os(key), &cwd)
    }

    /// `from_env` over an arbitrary variable lookup and working directory; lets tests avoid
    /// mutating the environment.
    fn from_lookup(
        lookup: impl Fn(&str) -> Option<OsString>,
        cwd: &Path,
    ) -> Result<Self, NoHomeDir> {
        let var = |key: &str| {
            lookup(key)
                .filter(|v| !v.is_empty())
                .map(|v| cwd.join(PathBuf::from(v)))
        };
        Self::resolve(PathOverrides {
            config_dir: var("RAIL_CONFIG_DIR"),
            data_dir: var("RAIL_DATA_DIR"),
            cache_dir: var("RAIL_CACHE_DIR"),
            state_dir: var("RAIL_STATE_DIR"),
            runtime_dir: var("RAIL_RUNTIME_DIR"),
        })
    }

    pub fn resolve(overrides: PathOverrides) -> Result<Self, NoHomeDir> {
        let dirs = ProjectDirs::from("", "", "rail");
        let config_dir = match overrides.config_dir {
            Some(p) => p,
            None => need(&dirs)?.config_dir().to_path_buf(),
        };
        let data_dir = match overrides.data_dir {
            Some(p) => p,
            None => need(&dirs)?.data_dir().to_path_buf(),
        };
        let cache_dir = match overrides.cache_dir {
            Some(p) => p,
            None => need(&dirs)?.cache_dir().to_path_buf(),
        };
        let state_dir = match overrides.state_dir {
            Some(p) => p,
            // `state_dir()` is Linux-only; elsewhere use the data dir itself.
            None => need(&dirs)?
                .state_dir()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| data_dir.clone()),
        };
        let runtime_dir = match overrides.runtime_dir {
            Some(p) => p,
            None => runtime_or_state(need(&dirs)?.runtime_dir(), &state_dir),
        };
        Ok(Self {
            config_dir,
            data_dir,
            cache_dir,
            state_dir,
            runtime_dir,
        })
    }

    /// Everything under one root. Used by tests.
    pub fn all_in(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
            runtime_dir: root.join("run"),
        }
    }

    pub fn config_file(&self) -> PathBuf {
        self.config_dir.join("config.toml")
    }

    pub fn db_file(&self) -> PathBuf {
        self.data_dir.join("rail.db")
    }

    pub fn secrets_file(&self) -> PathBuf {
        self.data_dir.join("secrets.json")
    }

    pub fn compose_dir(&self) -> PathBuf {
        self.cache_dir.join("compose")
    }

    pub fn attachments_dir(&self) -> PathBuf {
        self.cache_dir.join("attachments")
    }

    /// The attachments imported Gmail drafts own: one private directory per import, named by
    /// its client token. Data, not cache: a draft's files must outlive a cache clean.
    pub fn draft_files_dir(&self) -> PathBuf {
        self.data_dir.join("draft-files")
    }

    /// The directory an import with `token` owns: `draft_files_dir()/<token>`. None unless
    /// `token` passes the client-token rule, so it is one plain path segment.
    pub fn import_dir(&self, token: &str) -> Option<PathBuf> {
        import_dir_in(&self.draft_files_dir(), token)
    }

    pub fn log_dir(&self) -> &Path {
        &self.state_dir
    }

    /// The daemon's Unix socket.
    pub fn socket_file(&self) -> PathBuf {
        self.runtime_dir.join("rail.sock")
    }

    /// The lock a client holds while it starts the daemon.
    pub fn lock_file(&self) -> PathBuf {
        self.runtime_dir.join(SPAWN_LOCK_NAME)
    }

    /// The lock the running daemon holds for its whole life.
    pub fn daemon_lock_file(&self) -> PathBuf {
        self.runtime_dir.join(DAEMON_LOCK_NAME)
    }
}

/// The lock a client holds while it starts the daemon, in the runtime directory.
pub const SPAWN_LOCK_NAME: &str = "rail.lock";
/// The lock the running daemon holds for its whole life, in the runtime directory.
pub const DAEMON_LOCK_NAME: &str = "daemon.lock";

/// `$XDG_RUNTIME_DIR/rail` where there is one (Linux), else the state directory (macOS, and
/// Linux sessions without a runtime directory).
fn runtime_or_state(runtime: Option<&Path>, state: &Path) -> PathBuf {
    runtime.map_or_else(|| state.to_path_buf(), Path::to_path_buf)
}

/// `dir` with symlinks resolved, for comparing two spellings of a directory (macOS's `/var` is
/// `/private/var`; dotfile setups link config directories). Walked one component at a time:
/// each existing prefix is resolved with `canonicalize`, `.` is dropped, and `..` steps up
/// from the already resolved prefix, so a part that does not exist yet is appended as written
/// to its nearest existing ancestor. A relative `dir` is taken against the working directory;
/// a part that cannot be resolved is kept lexically.
pub fn canonical_dir(dir: &Path) -> PathBuf {
    use std::path::Component;
    let absolute = std::env::current_dir().unwrap_or_default().join(dir);
    let mut resolved = PathBuf::new();
    for part in absolute.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Prefix(_) | Component::RootDir => resolved.push(part),
            Component::Normal(name) => {
                let next = resolved.join(name);
                resolved = std::fs::canonicalize(&next).unwrap_or(next);
            }
        }
    }
    resolved
}

/// The user's download directory from an injected environment: `xdg_download` when given,
/// else `home/Downloads`. `None` without either. On Linux `xdg_download` is the value of the
/// `XDG_DOWNLOAD_DIR` **entry of `~/.config/user-dirs.dirs`** — not an environment variable —
/// which `directories` reads; the argument keeps this function pure so tests never mutate the
/// environment (`unsafe` is forbidden, so `std::env::set_var` is not available).
#[must_use]
pub fn download_dir_from(home: Option<&Path>, xdg_download: Option<&Path>) -> Option<PathBuf> {
    xdg_download
        .map(Path::to_path_buf)
        .or_else(|| home.map(|home| home.join("Downloads")))
}

/// `download_dir_from` over this process's environment, through `directories::UserDirs`.
#[must_use]
pub fn download_dir() -> Option<PathBuf> {
    let dirs = directories::UserDirs::new();
    download_dir_from(
        dirs.as_ref().map(directories::UserDirs::home_dir),
        dirs.as_ref().and_then(directories::UserDirs::download_dir),
    )
}

/// Where a save lands. `configured` being `~` alone, or starting `~/`, is resolved against
/// `home`; an empty `configured` counts as unset, as an empty `RAIL_*_DIR` does. Anything
/// else — and a `~` with no home to stand for — is returned **as typed** and never made
/// absolute against a working directory, so a relative, `~user/...` or unexpandable `~` value
/// reaches the daemon unchanged and is refused by name rather than quietly replaced by the
/// download directory. Unset falls back to `download_dir_from`.
#[must_use]
pub fn attachment_target(
    configured: Option<&Path>,
    home: Option<&Path>,
    xdg_download: Option<&Path>,
) -> Option<PathBuf> {
    match configured.filter(|path| !path.as_os_str().is_empty()) {
        Some(path) => Some(expand_tilde(path, home)),
        None => download_dir_from(home, xdg_download),
    }
}

/// `path` with a leading `~` — alone or followed by `/` — replaced by `home`; anything else,
/// and a `~` when there is no `home`, exactly as typed, so a relative or `~user/...` value is
/// never resolved here.
fn expand_tilde(path: &Path, home: Option<&Path>) -> PathBuf {
    let text = path.to_string_lossy();
    match (home, text.strip_prefix('~')) {
        (Some(home), Some("")) => home.to_path_buf(),
        (Some(home), Some(rest)) if rest.starts_with('/') => home.join(&rest[1..]),
        _ => path.to_path_buf(),
    }
}

/// Create `dir` (and missing parents) readable by the owner only. On unix a
/// directory that already exists is tightened to 0700.
#[cfg(unix)]
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(not(unix))]
pub fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Pre-create the file owner-only so it is never visible at umask mode.
#[cfg(unix)]
pub(crate) fn create_private_file(file: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(file)?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn create_private_file(_file: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
pub(crate) fn restrict_to_owner(file: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub(crate) fn restrict_to_owner(_file: &Path) -> std::io::Result<()> {
    Ok(())
}

/// Replace `path` with `bytes` atomically: the parent is created private, the
/// data goes to a 0600 sibling temp file which is synced and renamed over
/// `path`. A crash leaves either the old file or the new one, never a torn one.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    create_private_dir(parent_of(path))?;
    replace_private_atomic(path, bytes)
}

/// `write_private_atomic` into a directory that must already exist, leaving the directory's
/// own permissions as they are.
pub fn replace_private_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = parent_of(path);
    let tmp = temp_sibling(path);
    let result = write_synced_private(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result?;
    sync_dir(parent)
}

/// `<name>.<pid>-<n>.tmp` beside `path`: no two writes, in this process or another, share one.
fn temp_sibling(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(format!(".{}-{n}.tmp", std::process::id()));
    parent_of(path).join(name)
}

/// The directory `path` is in (`.` for a bare file name).
fn parent_of(path: &Path) -> &Path {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    }
}

fn write_synced_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    // A stale temp file from a crash keeps its old mode; tighten it explicitly.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(bytes)?;
    file.sync_all()
}

/// Make the rename durable. Directories cannot be opened for syncing on Windows.
#[cfg(unix)]
fn sync_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::File::open(dir)?.sync_all()
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) -> std::io::Result<()> {
    Ok(())
}

/// `root/<token>`, when `token` is 1 to 64 ASCII letters, digits, `-` or `_` (the client-token
/// rule): never a separator, `.`, `..` or an absolute path. The one place a token becomes a
/// directory.
pub(crate) fn import_dir_in(root: &Path, token: &str) -> Option<PathBuf> {
    crate::ops::actions::safe_token(token).then(|| root.join(token))
}

/// Remove `dir` and what is in it (a symlink is removed, never followed): whether it was
/// there. The caller logs a failure, without the path.
pub(crate) fn remove_dir_if_present(dir: &Path) -> std::io::Result<bool> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_checked_token_names_an_import_directory() {
        let root = Path::new("/d/draft-files");
        let long = "a".repeat(65);
        let ok = "a".repeat(64);
        assert_eq!(import_dir_in(root, "a-1_B"), Some(root.join("a-1_B")));
        assert_eq!(import_dir_in(root, &ok), Some(root.join(&ok)));
        for bad in [
            "", ".", "..", "a/b", "/abs", "a/", "a/.", "./a", "a\0b", "é", "a b", &long,
        ] {
            assert_eq!(import_dir_in(root, bad), None, "{bad:?}");
        }
    }

    /// AC-37: where a save lands, over an injected environment. Nothing here reads or
    /// mutates the process's own: `unsafe` is forbidden, so `set_var` is unavailable.
    #[test]
    fn attachment_target_expands_tilde_and_falls_back() {
        let home = Path::new("/h");
        let download = Path::new("/x/dl");
        let target = |configured: Option<&str>, xdg: Option<&Path>| {
            attachment_target(configured.map(Path::new), Some(home), xdg)
        };
        assert_eq!(target(Some("~/mail"), None), Some(PathBuf::from("/h/mail")));
        assert_eq!(target(Some("~"), None), Some(PathBuf::from("/h")));
        assert_eq!(target(Some("/abs"), None), Some(PathBuf::from("/abs")));
        // Left as typed, so the daemon refuses it by name instead of saving into whatever
        // directory `rail` happened to start in.
        assert_eq!(
            target(Some("rel/path"), None),
            Some(PathBuf::from("rel/path"))
        );
        assert_eq!(
            target(Some("~user/x"), None),
            Some(PathBuf::from("~user/x"))
        );
        // An empty key counts as unset, as an empty `RAIL_*_DIR` does.
        assert_eq!(
            target(Some(""), Some(download)),
            Some(download.to_path_buf())
        );
        assert_eq!(target(None, Some(download)), Some(download.to_path_buf()));
        assert_eq!(target(None, None), Some(PathBuf::from("/h/Downloads")));
        assert_eq!(attachment_target(None, None, None), None);
        // A `~` with no home to stand for is kept as typed, for the daemon to refuse by name,
        // never quietly swapped for the download directory.
        for tilde in ["~", "~/mail"] {
            for xdg in [None, Some(download)] {
                assert_eq!(
                    attachment_target(Some(Path::new(tilde)), None, xdg),
                    Some(PathBuf::from(tilde)),
                    "{tilde} with {xdg:?}"
                );
            }
        }

        assert_eq!(
            download_dir_from(Some(home), Some(download)),
            Some(download.to_path_buf())
        );
        assert_eq!(
            download_dir_from(Some(home), None),
            Some(PathBuf::from("/h/Downloads"))
        );
        assert_eq!(
            download_dir_from(None, Some(download)),
            Some(download.to_path_buf())
        );
        assert_eq!(download_dir_from(None, None), None);
    }

    #[cfg(unix)]
    #[test]
    fn canonical_dir_resolves_symlinks_for_existing_and_missing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
        assert_eq!(canonical_dir(&root.join("link")), root.join("real"));
        assert_eq!(
            canonical_dir(&root.join("link").join("not-yet").join("deeper")),
            root.join("real").join("not-yet").join("deeper")
        );
        assert_eq!(
            canonical_dir(&root.join("link").join("gone").join("..").join("x")),
            root.join("real").join("x")
        );
        assert_eq!(
            canonical_dir(&tmp.path().join("link").join("./y")),
            root.join("real").join("y")
        );
    }

    #[test]
    fn canonical_dir_keeps_different_dirs_different() {
        let tmp = tempfile::tempdir().unwrap();
        assert_ne!(
            canonical_dir(&tmp.path().join("a")),
            canonical_dir(&tmp.path().join("b"))
        );
        assert!(canonical_dir(Path::new("relative/dir")).is_absolute());
    }
    use crate::error::ErrorClass;
    use std::path::PathBuf;

    #[test]
    fn overrides_win_over_defaults() {
        let paths = Paths::resolve(PathOverrides {
            config_dir: Some(PathBuf::from("/c")),
            data_dir: Some(PathBuf::from("/d")),
            cache_dir: Some(PathBuf::from("/k")),
            state_dir: Some(PathBuf::from("/s")),
            runtime_dir: Some(PathBuf::from("/r")),
        })
        .unwrap();
        assert_eq!(paths.config_file(), PathBuf::from("/c/config.toml"));
        assert_eq!(paths.db_file(), PathBuf::from("/d/rail.db"));
        assert_eq!(paths.secrets_file(), PathBuf::from("/d/secrets.json"));
        assert_eq!(paths.compose_dir(), PathBuf::from("/k/compose"));
        assert_eq!(paths.attachments_dir(), PathBuf::from("/k/attachments"));
        assert_eq!(paths.draft_files_dir(), PathBuf::from("/d/draft-files"));
        assert_eq!(paths.log_dir(), Path::new("/s"));
        assert_eq!(paths.socket_file(), PathBuf::from("/r/rail.sock"));
        assert_eq!(paths.lock_file(), PathBuf::from("/r/rail.lock"));
        assert_eq!(paths.daemon_lock_file(), PathBuf::from("/r/daemon.lock"));
    }

    #[test]
    fn defaults_end_in_rail() {
        let paths = Paths::resolve(PathOverrides::default()).unwrap();
        for dir in [
            &paths.config_dir,
            &paths.data_dir,
            &paths.cache_dir,
            &paths.state_dir,
            &paths.runtime_dir,
        ] {
            assert_eq!(dir.file_name().unwrap(), "rail", "{dir:?}");
        }
    }

    #[test]
    fn all_in_puts_everything_under_one_root() {
        let paths = Paths::all_in(Path::new("/tmp/x"));
        assert_eq!(paths.config_dir, PathBuf::from("/tmp/x/config"));
        assert_eq!(paths.data_dir, PathBuf::from("/tmp/x/data"));
        assert_eq!(paths.cache_dir, PathBuf::from("/tmp/x/cache"));
        assert_eq!(paths.state_dir, PathBuf::from("/tmp/x/state"));
        assert_eq!(paths.runtime_dir, PathBuf::from("/tmp/x/run"));
    }

    #[test]
    fn runtime_dir_env_override_and_fallback() {
        let lookup = |key: &str| (key == "RAIL_RUNTIME_DIR").then(|| "/run/x".into());
        assert_eq!(
            Paths::from_lookup(lookup, Path::new("/cwd"))
                .unwrap()
                .runtime_dir,
            PathBuf::from("/run/x")
        );
        assert_eq!(
            runtime_or_state(Some(Path::new("/run/user/1000/rail")), Path::new("/s")),
            PathBuf::from("/run/user/1000/rail")
        );
        assert_eq!(runtime_or_state(None, Path::new("/s")), PathBuf::from("/s"));
    }

    #[test]
    fn relative_env_overrides_are_made_absolute_against_the_working_directory() {
        let lookup = |key: &str| match key {
            "RAIL_CONFIG_DIR" => Some("cfg".into()),
            "RAIL_DATA_DIR" => Some("../data".into()),
            "RAIL_CACHE_DIR" => Some("/abs/cache".into()),
            "RAIL_STATE_DIR" => Some("./state".into()),
            "RAIL_RUNTIME_DIR" => Some("run".into()),
            _ => None,
        };
        let paths = Paths::from_lookup(lookup, Path::new("/home/me/work")).unwrap();
        assert_eq!(paths.config_dir, PathBuf::from("/home/me/work/cfg"));
        assert_eq!(paths.data_dir, PathBuf::from("/home/me/work/../data"));
        assert_eq!(paths.cache_dir, PathBuf::from("/abs/cache"));
        assert_eq!(paths.state_dir, PathBuf::from("/home/me/work/./state"));
        assert_eq!(paths.runtime_dir, PathBuf::from("/home/me/work/run"));
        assert!(paths.config_dir.is_absolute() && paths.runtime_dir.is_absolute());
    }

    #[test]
    fn empty_env_override_is_treated_as_unset() {
        let lookup = |key: &str| match key {
            "RAIL_CONFIG_DIR" => Some(std::ffi::OsString::new()),
            "RAIL_DATA_DIR" => Some(std::ffi::OsString::from("/d")),
            _ => None,
        };
        let paths = Paths::from_lookup(lookup, Path::new("/cwd")).unwrap();
        let defaults = Paths::resolve(PathOverrides::default()).unwrap();
        assert_eq!(paths.config_dir, defaults.config_dir);
        assert_eq!(paths.data_dir, PathBuf::from("/d"));
        assert_ne!(paths.config_dir, PathBuf::new());
    }

    #[test]
    fn write_private_atomic_replaces_content_and_leaves_no_temp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("file.toml");
        write_private_atomic(&path, b"first, longer body\n").unwrap();
        write_private_atomic(&path, b"second\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second\n");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("file.toml")]);
    }

    #[cfg(unix)]
    #[test]
    fn private_helpers_set_and_tighten_modes() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let loose = dir.path().join("loose");
        std::fs::create_dir(&loose).unwrap();
        std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(&loose).unwrap();
        assert_eq!(mode(&loose), 0o700);

        let file = loose.join("f");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let stale_tmp = loose.join("f.tmp");
        std::fs::write(&stale_tmp, "stale").unwrap();
        std::fs::set_permissions(&stale_tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private_atomic(&file, b"new").unwrap();
        assert_eq!(mode(&file), 0o600);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        // A leftover from an older build's fixed temp name is never written through.
        assert_eq!(std::fs::read_to_string(&stale_tmp).unwrap(), "stale");
    }

    #[cfg(unix)]
    #[test]
    fn replace_private_atomic_keeps_the_directory_mode_and_needs_the_directory() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        let dir = tempfile::tempdir().unwrap();
        let shared = dir.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o755)).unwrap();
        let file = shared.join("f");
        replace_private_atomic(&file, b"new").unwrap();
        assert_eq!(mode(&shared), 0o755, "the directory is left alone");
        assert_eq!(mode(&file), 0o600);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new");
        let missing = dir.path().join("missing").join("f");
        assert!(replace_private_atomic(&missing, b"x").is_err());
    }

    #[test]
    fn every_atomic_write_gets_its_own_temp_file_beside_the_target() {
        let file = Path::new("/somewhere/config.toml");
        let (a, b) = (temp_sibling(file), temp_sibling(file));
        assert_ne!(a, b, "two writers never share a temp file");
        for tmp in [&a, &b] {
            assert_eq!(tmp.parent(), file.parent());
            let name = tmp.file_name().unwrap().to_string_lossy();
            assert!(
                name.starts_with("config.toml.") && name.ends_with(".tmp"),
                "{name}"
            );
        }
    }

    #[test]
    fn no_home_dir_is_permanent() {
        assert_eq!(NoHomeDir.class(), ErrorClass::Permanent);
    }
}
