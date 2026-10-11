use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

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
    /// Where `secrets = "1password"` keeps tokens, and how to run `op`.
    #[serde(default)]
    pub onepassword: Option<OnePasswordConfig>,
}

/// The `[onepassword]` table.
#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OnePasswordConfig {
    /// The vault Rail keeps its items in.
    pub vault: String,
    /// The 1Password account, when several are signed in.
    #[serde(default)]
    pub account: Option<String>,
    /// The `op` binary; `op` on PATH when absent. Set by hand only.
    #[serde(default)]
    pub op_path: Option<PathBuf>,
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

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SecretsBackend {
    Keyring,
    File,
    /// Items in a 1Password vault, through the `op` CLI.
    #[serde(rename = "1password")]
    OnePassword,
}

impl SecretsBackend {
    /// The spelling in `config.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keyring => "keyring",
            Self::File => "file",
            Self::OnePassword => "1password",
        }
    }
}

#[derive(Clone, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AccountConfig {
    pub name: String,
    pub kind: AccountKind,
    #[serde(deserialize_with = "plain_address")]
    pub address: String,
    #[serde(default, deserialize_with = "plain_color")]
    pub color: Option<String>,
    pub client_id: String,
    #[serde(default)]
    pub client_secret: Option<String>,
    /// Microsoft only. Defaults to `common`.
    #[serde(default, deserialize_with = "plain_tenant")]
    pub tenant: Option<String>,
    #[serde(default)]
    pub signature: Option<PathBuf>,
    /// What an AI agent may do with this account's mail over MCP. Unset means `confirm`.
    #[serde(default)]
    pub ai_send: Option<AiSend>,
}

/// True when `text` holds no whitespace, no control character and no Unicode format
/// character (general category Cf: bidi overrides such as U+202E, zero-width joiners and
/// spaces, the byte-order mark). An account's address, colour and tenant are shown on the
/// terminal and in confirmations; none of these belongs in any of them.
pub fn is_plain(text: &str) -> bool {
    !text
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || is_format_char(c))
}

/// True for a Unicode format character (general category Cf): invisible, and able to reorder
/// or hide what is shown around it.
pub fn is_format_char(c: char) -> bool {
    use icu_properties::CodePointMapData;
    use icu_properties::props::GeneralCategory;
    CodePointMapData::<GeneralCategory>::new().get(c) == GeneralCategory::Format
}

/// Why a value that is not `is_plain` is refused. Never quotes the value.
pub fn not_plain(field: &str) -> String {
    format!("the {field} must not contain spaces, control characters or format characters")
}

/// A plain string, or a deserialization error naming `field` (the parser adds the position).
fn plain<'de, D: serde::Deserializer<'de>>(field: &str, d: D) -> Result<String, D::Error> {
    let text = String::deserialize(d)?;
    if is_plain(&text) {
        Ok(text)
    } else {
        Err(serde::de::Error::custom(not_plain(field)))
    }
}

fn plain_address<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    plain("address", d)
}

fn plain_color<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    plain("color", d).map(Some)
}

fn plain_tenant<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    plain("tenant", d).map(Some)
}

impl AccountConfig {
    /// The account's send policy, `confirm` when the key is absent.
    pub fn ai_send(&self) -> AiSend {
        self.ai_send.unwrap_or_default()
    }
}

/// What an agent may do through MCP with an account's mail: send, reply, forward and delete.
/// Parsed and reported now; enforced when the send tools arrive.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AiSend {
    /// The user confirms each action through MCP elicitation; without elicitation, drafts only.
    #[default]
    Confirm,
    /// Drafts may be created and edited, never sent.
    Drafts,
    /// Rely on the MCP client's own per-call approval prompt. The weakest option.
    Client,
}

impl AiSend {
    /// The value as written in `config.toml`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Drafts => "drafts",
            Self::Client => "client",
        }
    }
}

/// Hides `client_secret` so `{:?}` and tracing fields never print it.
impl std::fmt::Debug for AccountConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountConfig")
            .field("name", &self.name)
            .field("kind", &self.kind)
            .field("address", &self.address)
            .field("color", &self.color)
            .field("client_id", &self.client_id)
            .field(
                "client_secret",
                &self.client_secret.as_ref().map(|_| "<redacted>"),
            )
            .field("tenant", &self.tenant)
            .field("signature", &self.signature)
            .field("ai_send", &self.ai_send)
            .finish()
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Parked item 3: bidi overrides and zero-width characters are Unicode format (Cf)
    /// characters; an address holding one could display as another.
    #[test]
    fn format_characters_are_not_plain() {
        for c in [
            '\u{202E}',
            '\u{200B}',
            '\u{200D}',
            '\u{2066}',
            '\u{FEFF}',
            '\u{E0001}',
        ] {
            assert!(!is_plain(&format!("me{c}@example.com")), "{:?}", c);
        }
        assert!(is_plain("ünïcödé@example.com"));
    }

    #[test]
    fn account_debug_hides_the_client_secret() {
        let account = AccountConfig {
            client_secret: Some("GOCSPX-very-secret".into()),
            ..crate::test_support::sample_account()
        };
        let debug = format!("{account:?}");
        assert!(!debug.contains("GOCSPX-very-secret"), "{debug}");
        assert!(debug.contains("<redacted>"), "{debug}");
        assert!(debug.contains("me@example.com"), "{debug}");
    }

    #[test]
    fn ai_send_defaults_to_confirm_and_parses_each_value() {
        use crate::config::parse;
        use std::path::Path;
        let account = |extra: &str| {
            format!(
                "[[accounts]]\nname = \"w\"\nkind = \"gmail\"\naddress = \"w@example.com\"\nclient_id = \"c\"\n{extra}"
            )
        };
        let unset = parse(&account(""), Path::new("c.toml")).unwrap();
        assert_eq!(unset.accounts[0].ai_send, None);
        assert_eq!(unset.accounts[0].ai_send(), AiSend::Confirm);
        for (text, expected) in [
            ("confirm", AiSend::Confirm),
            ("drafts", AiSend::Drafts),
            ("client", AiSend::Client),
        ] {
            let line = format!("ai_send = \"{text}\"\n");
            let config = parse(&account(&line), Path::new("c.toml")).unwrap();
            assert_eq!(config.accounts[0].ai_send(), expected);
            assert_eq!(expected.as_str(), text);
        }
    }

    #[test]
    fn an_unknown_ai_send_value_reports_its_line() {
        use crate::config::{ConfigError, parse};
        use std::path::Path;
        let text = "[[accounts]]\nname = \"w\"\nkind = \"gmail\"\naddress = \"w@example.com\"\nclient_id = \"c\"\nai_send = \"always\"\n";
        let err = parse(text, Path::new("/x/config.toml")).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { line: 6, .. }), "{err}");
        assert!(err.to_string().contains("always"), "{err}");
    }
}
