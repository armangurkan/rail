//! The lock every read, edit and replace of `config.toml` holds. It is a file lock, so the
//! daemon and any number of TUIs exclude each other, not only threads in one process.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::paths::create_private_dir;

/// Held while `config.toml` is read, edited and replaced; dropping it releases the lock.
#[derive(Debug)]
pub struct ConfigLock {
    _file: File,
}

/// `config.lock` beside `config` as named, not where a symlink points: every writer names the
/// same path, so they all agree on the lock.
pub fn lock_path(config: &Path) -> PathBuf {
    config.with_file_name("config.lock")
}

/// Wait for the lock beside `config`. A missing directory is created private and the lock file
/// owner-only; an existing directory is left as it is.
pub fn lock_config(config: &Path) -> std::io::Result<ConfigLock> {
    let path = lock_path(config);
    if let Some(dir) = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty() && !d.is_dir())
    {
        create_private_dir(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(&path)?;
    // By path: `file.lock()` would resolve to `std::fs::File::lock`, newer than our MSRV.
    fs4::FileExt::lock(&file)?;
    Ok(ConfigLock { _file: file })
}
