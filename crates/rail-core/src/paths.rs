//! Where Rail keeps its files. XDG on Linux, platform dirs elsewhere,
//! `RAIL_*_DIR` environment overrides everywhere.

use std::path::{Path, PathBuf};

use directories::ProjectDirs;

use crate::error::ErrorClass;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
}

#[derive(Debug, Default, Clone)]
pub struct PathOverrides {
    pub config_dir: Option<PathBuf>,
    pub data_dir: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub state_dir: Option<PathBuf>,
}

#[derive(Debug, thiserror::Error)]
#[error(
    "cannot determine a home directory; set RAIL_CONFIG_DIR, RAIL_DATA_DIR, RAIL_CACHE_DIR and RAIL_STATE_DIR"
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
    pub fn from_env() -> Result<Self, NoHomeDir> {
        let var = |key: &str| std::env::var_os(key).map(PathBuf::from);
        Self::resolve(PathOverrides {
            config_dir: var("RAIL_CONFIG_DIR"),
            data_dir: var("RAIL_DATA_DIR"),
            cache_dir: var("RAIL_CACHE_DIR"),
            state_dir: var("RAIL_STATE_DIR"),
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
        Ok(Self {
            config_dir,
            data_dir,
            cache_dir,
            state_dir,
        })
    }

    /// Everything under one root. Used by tests.
    pub fn all_in(root: &Path) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
            state_dir: root.join("state"),
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

    pub fn log_dir(&self) -> &Path {
        &self.state_dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorClass;
    use std::path::PathBuf;

    #[test]
    fn overrides_win_over_defaults() {
        let paths = Paths::resolve(PathOverrides {
            config_dir: Some(PathBuf::from("/c")),
            data_dir: Some(PathBuf::from("/d")),
            cache_dir: Some(PathBuf::from("/k")),
            state_dir: Some(PathBuf::from("/s")),
        })
        .unwrap();
        assert_eq!(paths.config_file(), PathBuf::from("/c/config.toml"));
        assert_eq!(paths.db_file(), PathBuf::from("/d/rail.db"));
        assert_eq!(paths.secrets_file(), PathBuf::from("/d/secrets.json"));
        assert_eq!(paths.compose_dir(), PathBuf::from("/k/compose"));
        assert_eq!(paths.attachments_dir(), PathBuf::from("/k/attachments"));
        assert_eq!(paths.log_dir(), Path::new("/s"));
    }

    #[test]
    fn defaults_end_in_rail() {
        let paths = Paths::resolve(PathOverrides::default()).unwrap();
        for dir in [
            &paths.config_dir,
            &paths.data_dir,
            &paths.cache_dir,
            &paths.state_dir,
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
    }

    #[test]
    fn no_home_dir_is_permanent() {
        assert_eq!(NoHomeDir.class(), ErrorClass::Permanent);
    }
}
