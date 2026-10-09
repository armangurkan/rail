use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::model::Config;
use crate::error::ErrorClass;

const MIN_POLL_INTERVAL: Duration = Duration::from_secs(5);

// `PathBuf` has no `Display`, so paths are formatted through `.display()`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no config file at {}; run `rail account add` to create one", .path.display())]
    Missing { path: PathBuf },
    #[error("cannot read {}: {source}", .path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{}:{line}:{col}: {message}", .path.display())]
    Parse {
        path: PathBuf,
        line: usize,
        col: usize,
        message: String,
    },
    #[error("{}: {message}", .path.display())]
    Invalid { path: PathBuf, message: String },
}

impl ConfigError {
    /// A bad or missing config never fixes itself by retrying.
    pub fn class(&self) -> ErrorClass {
        ErrorClass::Permanent
    }
}

pub fn load(path: &Path) -> Result<Config, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(ConfigError::Missing {
                path: path.to_path_buf(),
            });
        }
        Err(source) => {
            return Err(ConfigError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    parse(&text, path)
}

/// Parse and validate config text. `path` is only used in error messages.
pub fn parse(text: &str, path: &Path) -> Result<Config, ConfigError> {
    let config: Config = toml::from_str(text).map_err(|e| {
        let (line, col) = e.span().map_or((0, 0), |s| line_col(text, s.start));
        ConfigError::Parse {
            path: path.to_path_buf(),
            line,
            col,
            message: e.message().to_string(),
        }
    })?;
    validate(&config).map_err(|message| ConfigError::Invalid {
        path: path.to_path_buf(),
        message,
    })?;
    Ok(config)
}

/// 1-based line and column of a byte offset.
pub fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let before = &text[..offset.min(text.len())];
    let line = before.matches('\n').count() + 1;
    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
    (line, col)
}

fn validate(config: &Config) -> Result<(), String> {
    let mut names = HashSet::new();
    let mut addresses = HashSet::new();
    for account in &config.accounts {
        let name_ok = !account.name.is_empty()
            && account
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !name_ok {
            return Err(format!(
                "account name {:?} must be non-empty and use only letters, digits, '-' or '_'",
                account.name
            ));
        }
        if !names.insert(account.name.as_str()) {
            return Err(format!("duplicate account name {:?}", account.name));
        }
        if !account.address.contains('@') {
            return Err(format!(
                "account {:?}: {:?} is not an email address",
                account.name, account.address
            ));
        }
        if !addresses.insert(account.address.to_ascii_lowercase()) {
            return Err(format!("duplicate account address {:?}", account.address));
        }
    }
    if config.general.poll_interval < MIN_POLL_INTERVAL {
        return Err("general.poll_interval must be at least 5s".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::{AccountKind, Layout, SecretsBackend};
    use std::time::Duration;

    const FULL: &str = r#"
[general]
editor = "nvim"
poll_interval = "2m"
body_cache_days = 10
history_days = 365
layout = "split"
split_threshold = 120
secrets = "file"

[[accounts]]
name = "work"
kind = "gmail"
address = "me@example.com"
color = "blue"
client_id = "abc.apps.googleusercontent.com"
client_secret = "shh"
signature = "~/.config/rail/sig.txt"

[[accounts]]
name = "corp"
kind = "m365"
address = "me@corp.com"
client_id = "xyz"
tenant = "common"

[keys]
archive = "e"

[theme]
unread = { bold = true }
"#;

    fn p(text: &str) -> Result<Config, ConfigError> {
        parse(text, Path::new("/etc/rail/config.toml"))
    }

    #[test]
    fn parses_full_example() {
        let c = p(FULL).unwrap();
        assert_eq!(c.general.editor.as_deref(), Some("nvim"));
        assert_eq!(c.general.poll_interval, Duration::from_secs(120));
        assert_eq!(c.general.body_cache_days, 10);
        assert_eq!(c.general.history_days, 365);
        assert_eq!(c.general.layout, Layout::Split);
        assert_eq!(c.general.split_threshold, 120);
        assert_eq!(c.general.secrets, SecretsBackend::File);
        assert_eq!(c.accounts.len(), 2);
        assert_eq!(c.accounts[0].kind, AccountKind::Gmail);
        assert_eq!(c.accounts[0].client_secret.as_deref(), Some("shh"));
        assert_eq!(c.accounts[1].kind, AccountKind::M365);
        assert_eq!(c.accounts[1].tenant.as_deref(), Some("common"));
        assert_eq!(c.keys.get("archive").map(String::as_str), Some("e"));
        assert!(c.theme.contains_key("unread"));
    }

    #[test]
    fn empty_config_uses_defaults() {
        let c = p("").unwrap();
        assert_eq!(c.general.poll_interval, Duration::from_secs(60));
        assert_eq!(c.general.body_cache_days, 30);
        assert_eq!(c.general.history_days, 0);
        assert_eq!(c.general.layout, Layout::Auto);
        assert_eq!(c.general.split_threshold, 140);
        assert_eq!(c.general.secrets, SecretsBackend::Keyring);
        assert!(c.accounts.is_empty());
    }

    #[test]
    fn unknown_key_reports_line_number() {
        let text = "[general]\neditor = \"vi\"\nbogus = 1\n";
        let err = p(text).unwrap_err();
        match err {
            ConfigError::Parse {
                line, ref message, ..
            } => {
                assert_eq!(line, 3, "{message}");
                assert!(message.contains("bogus"), "{message}");
            }
            ref other => panic!("expected Parse, got {other:?}"),
        }
        assert!(err.to_string().starts_with("/etc/rail/config.toml:3:"));
    }

    #[test]
    fn duplicate_account_name_is_invalid() {
        let text = r#"
[[accounts]]
name = "a"
kind = "gmail"
address = "x@example.com"
client_id = "1"
[[accounts]]
name = "a"
kind = "gmail"
address = "y@example.com"
client_id = "2"
"#;
        let err = p(text).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid { .. }));
        assert!(err.to_string().contains("duplicate account name"), "{err}");
    }

    #[test]
    fn bad_name_and_bad_address_are_invalid() {
        let bad_name = "[[accounts]]\nname = \"has space\"\nkind = \"gmail\"\naddress = \"x@e.com\"\nclient_id = \"1\"\n";
        assert!(
            p(bad_name)
                .unwrap_err()
                .to_string()
                .contains("account name")
        );
        let bad_addr = "[[accounts]]\nname = \"ok\"\nkind = \"gmail\"\naddress = \"nope\"\nclient_id = \"1\"\n";
        assert!(
            p(bad_addr)
                .unwrap_err()
                .to_string()
                .contains("not an email address")
        );
    }

    #[test]
    fn poll_interval_floor_is_five_seconds() {
        let err = p("[general]\npoll_interval = \"1s\"\n").unwrap_err();
        assert!(err.to_string().contains("poll_interval"), "{err}");
    }

    #[test]
    fn load_reports_missing_file_with_hint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let err = load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Missing { .. }));
        assert!(err.to_string().contains("rail account add"), "{err}");
    }

    #[test]
    fn line_col_counts_from_one() {
        assert_eq!(line_col("ab\ncd", 0), (1, 1));
        assert_eq!(line_col("ab\ncd", 3), (2, 1));
        assert_eq!(line_col("ab\ncd", 4), (2, 2));
    }

    #[test]
    fn every_error_is_permanent() {
        use crate::error::ErrorClass;
        let path = PathBuf::from("/x");
        let missing = ConfigError::Missing { path: path.clone() };
        let invalid = ConfigError::Invalid {
            path,
            message: "m".into(),
        };
        assert_eq!(missing.class(), ErrorClass::Permanent);
        assert_eq!(invalid.class(), ErrorClass::Permanent);
    }
}
