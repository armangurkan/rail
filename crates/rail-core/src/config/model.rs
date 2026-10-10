use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub general: General,
    #[serde(default)]
    pub accounts: Vec<AccountConfig>,
    #[serde(default)]
    pub keys: BTreeMap<String, String>,
    /// Free-form until the TUI phase defines the theme schema.
    #[serde(default)]
    pub theme: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields, default)]
pub struct General {
    pub editor: Option<String>,
    #[serde(with = "humantime_serde")]
    pub poll_interval: Duration,
    pub body_cache_days: u32,
    /// 0 means the whole mailbox.
    pub history_days: u32,
    pub layout: Layout,
    pub split_threshold: u16,
    pub secrets: SecretsBackend,
}

impl Default for General {
    fn default() -> Self {
        Self {
            editor: None,
            poll_interval: Duration::from_secs(60),
            body_cache_days: 30,
            history_days: 0,
            layout: Layout::Auto,
            split_threshold: 140,
            secrets: SecretsBackend::Keyring,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Layout {
    Auto,
    Split,
    Stack,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SecretsBackend {
    Keyring,
    File,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub name: String,
    pub kind: AccountKind,
    pub address: String,
    #[serde(default)]
    pub color: Option<String>,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Microsoft only. Defaults to `common`.
    #[serde(default)]
    pub tenant: Option<String>,
    #[serde(default)]
    pub signature: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Gmail,
    M365,
}

impl AccountKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Gmail => "gmail",
            Self::M365 => "m365",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "gmail" => Some(Self::Gmail),
            "m365" => Some(Self::M365),
            _ => None,
        }
    }
}
