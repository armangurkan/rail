//! The one error type every operation returns. It crosses the daemon socket and reaches MCP
//! clients, so it carries text only: never a token, a client secret or an authorization code.
//!
//! One rule governs every conversion: text Rail did not write (a provider's description, OS or
//! transport text, a parser's message, a keyring's message) goes into `detail`; `message` and
//! `hint` hold only Rail's fixed text plus values from the request or Rail's own state.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::error::Category;

use super::confirm::ConfirmPlan;
use super::untrusted::Untrusted;
use crate::auth::{AuthError, SecretError};
use crate::config::ConfigError;
use crate::error::ErrorClass;
use crate::onboarding::{PRESET_TOO_LARGE, PresetError};
use crate::store::StoreError;

/// A stable, machine-readable error code. Codes may be added; existing ones never change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// `config.toml` is missing, unreadable or invalid.
    Config,
    /// The request is wrong: a missing field, a bad value, a conflicting combination.
    InvalidInput,
    /// No account, preset or pending operation by that name.
    NotFound,
    /// The account or address is already configured.
    Duplicate,
    /// The token store (keyring or file) failed.
    Secrets,
    /// The SQLite store failed.
    Store,
    /// The provider refused the login or a token request.
    Auth,
    /// Microsoft says an administrator has to approve Rail first.
    ConsentRequired,
    /// Network trouble talking to a provider or a preset URL.
    Network,
    /// A preset could not be read, fetched or parsed.
    Preset,
    /// The operation was cancelled.
    Cancelled,
    /// The operation needs explicit confirmation and did not get it.
    NotConfirmed,
    /// Waiting took too long.
    Timeout,
    /// The daemon, its socket or its protocol failed.
    Daemon,
    /// A bug in Rail.
    Internal,
}

/// A failed operation. `message` is Rail's own text, written for a person (or a model) to read;
/// `detail`, when present, is text from outside Rail (a provider's error description), kept
/// apart so nothing mistakes it for Rail's words; `hint` says what to do next.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, thiserror::Error)]
#[error("{message}")]
pub struct OpError {
    pub code: ErrorCode,
    pub class: ErrorClass,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<Untrusted<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Set on a `NotConfirmed` refusal: the text to show the person and the plan digest the
    /// confirmed request must carry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confirmation: Option<Box<ConfirmPlan>>,
}

impl OpError {
    pub fn new(code: ErrorCode, class: ErrorClass, message: impl Into<String>) -> Self {
        Self {
            code,
            class,
            message: message.into(),
            detail: None,
            hint: None,
            confirmation: None,
        }
    }

    /// A change that needs the person's confirmation of `plan` and did not get it (or got it
    /// for another plan). Nothing was changed.
    pub fn not_confirmed(message: impl Into<String>, plan: ConfirmPlan) -> Self {
        let mut error = Self::new(ErrorCode::NotConfirmed, ErrorClass::Permanent, message);
        error.confirmation = Some(Box::new(plan));
        error
    }

    /// Attach text that came from outside Rail, such as a provider's error description.
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(Untrusted::new(detail.into()));
        self
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidInput, ErrorClass::Permanent, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, ErrorClass::Permanent, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, ErrorClass::Permanent, message)
    }

    /// An I/O failure while doing `what`. Retrying may help, so it is transient. The operating
    /// system's text is outside text: it goes into `detail`.
    pub fn io(code: ErrorCode, what: &str, error: &std::io::Error) -> Self {
        Self::new(code, ErrorClass::Transient, what).with_detail(error.to_string())
    }

    /// A request whose parameters do not fit the operation. Only the kind and position of the
    /// mismatch are reported, plus the field name for a missing or unknown field: serde's own
    /// text for a wrong type quotes the value, which may be a client secret.
    pub fn invalid_params(error: &serde_json::Error) -> Self {
        let text = error.to_string();
        let named = text.starts_with("missing field") || text.starts_with("unknown field");
        let detail = if named {
            text.split(" at line").next().unwrap_or("").to_string()
        } else {
            match error.classify() {
                Category::Io => "an I/O error".to_string(),
                Category::Syntax => "a syntax error".to_string(),
                Category::Data => "a value of the wrong type".to_string(),
                Category::Eof => "an unexpected end of data".to_string(),
            }
        };
        Self::invalid_input(format!(
            "the request does not match the operation's parameters: {detail}"
        ))
    }

    /// The message, the outside detail and the hint, for a person at a terminal. Never for a
    /// model: MCP summaries quote `detail` as outside text instead.
    pub fn text(&self) -> String {
        let mut text = self.message.clone();
        if let Some(detail) = &self.detail {
            text = format!("{text}: {}", detail.get());
        }
        if let Some(hint) = &self.hint {
            text = format!("{text}\nhint: {hint}");
        }
        text
    }
}

/// The config parser's and the file system's text goes into `detail`; the message names the
/// file and, for a syntax error, the line and column.
impl From<ConfigError> for OpError {
    fn from(error: ConfigError) -> Self {
        let class = error.class();
        let config = |message: String| Self::new(ErrorCode::Config, class, message);
        match error {
            ConfigError::Missing { path } => config(format!("no config file at {}", path.display()))
                .with_hint("run `rail setup`, or add an account with `rail account add` or the rail_account_add tool"),
            ConfigError::Io { path, source } => {
                config(format!("cannot read {}", path.display())).with_detail(source.to_string())
            }
            ConfigError::Parse {
                path,
                line,
                col,
                message,
            } => config(format!(
                "{}:{line}:{col}: the config is not valid",
                path.display()
            ))
            .with_detail(message),
            ConfigError::Invalid { path, message } => {
                config(format!("{}: the config is not valid", path.display())).with_detail(message)
            }
        }
    }
}

/// The secret stores' own text (the OS keyring's message, an I/O error) is outside text: it
/// goes into `detail` behind Rail's fixed message.
impl From<SecretError> for OpError {
    fn from(error: SecretError) -> Self {
        let class = error.class();
        let fixed = |message: String, detail: &str| {
            Self::new(ErrorCode::Secrets, class, message).with_detail(detail)
        };
        match &error {
            SecretError::KeyringUnavailable(text) => fixed(
                "the OS keyring is unavailable; set `secrets = \"file\"` under [general] in config.toml to keep tokens in a private file instead".to_string(),
                text,
            ),
            SecretError::Keyring(text) => {
                fixed("the OS keyring reported an error".to_string(), text)
            }
            SecretError::Io { path, source } => fixed(
                format!("cannot use the secrets file {}", path.display()),
                &source.to_string(),
            ),
            SecretError::Corrupt { path, message } => fixed(
                format!("the secrets file {} is corrupt", path.display()),
                message,
            ),
            SecretError::Encoding(text) => fixed("the stored token data is corrupt".to_string(), text),
            SecretError::OnePasswordLocked(text) => fixed(
                "1Password is not unlocked or not signed in".to_string(),
                text,
            )
            .with_hint("unlock the 1Password app or run `op signin`, then try again"),
            SecretError::OnePasswordTimeout => Self::new(
                ErrorCode::Secrets,
                class,
                "1Password did not answer in time",
            )
            .with_hint("if 1Password is waiting for you to unlock it, unlock it and try again"),
            SecretError::OnePasswordMissing { program, message } => fixed(
                format!("cannot run the 1Password CLI {program:?}"),
                message,
            )
            .with_hint("install the 1Password CLI (`op`), or set op_path under [onepassword] in config.toml"),
            SecretError::OnePasswordUnsafe { path, reason } => fixed(
                format!("refusing to run the 1Password CLI at {path}"),
                reason,
            )
            .with_hint("fix op_path under [onepassword] in config.toml by hand"),
            SecretError::OnePasswordNoItem(text) => {
                fixed("1Password has no such item".to_string(), text)
            }
            SecretError::OnePasswordInvalid(text) => {
                Self::new(ErrorCode::Secrets, class, text.clone())
            }
            SecretError::OnePasswordOutputTooLarge => {
                Self::new(ErrorCode::Secrets, class, "op's output exceeded 1 MiB")
            }
            SecretError::OnePasswordIo { what, message } => {
                fixed(format!("could not {what} the 1Password CLI"), message)
            }
            SecretError::OnePassword(text) => {
                fixed("1Password reported an error".to_string(), text)
            }
        }
    }
}

/// Write `config.toml` (owner-only, atomically), mapping a failure to a config error. Every
/// operation that edits the config goes through here.
pub(crate) fn write_config_text(path: &std::path::Path, text: &str) -> Result<(), OpError> {
    crate::paths::write_private_atomic(path, text.as_bytes()).map_err(|e| {
        OpError::io(
            ErrorCode::Config,
            &format!("writing {}", path.display()),
            &e,
        )
    })
}

/// SQLite's and the file system's own text goes into `detail`.
impl From<StoreError> for OpError {
    fn from(error: StoreError) -> Self {
        let class = error.class();
        match &error {
            StoreError::Sqlite(e) => {
                Self::new(ErrorCode::Store, class, "the store failed").with_detail(e.to_string())
            }
            StoreError::CreateDir { path, source } => Self::new(
                ErrorCode::Store,
                class,
                format!("cannot create {}", path.display()),
            )
            .with_detail(source.to_string()),
            StoreError::TooNew { .. } => Self::new(ErrorCode::Store, class, error.to_string()),
        }
    }
}

/// Provider text (an OAuth `error_description`, a token endpoint's error body) and transport
/// text become the untrusted `detail`; the message is Rail's own fixed text.
impl From<AuthError> for OpError {
    fn from(error: AuthError) -> Self {
        if let AuthError::Secret(secret) = error {
            return secret.into();
        }
        let code = if error.consent_required() {
            ErrorCode::ConsentRequired
        } else {
            match &error {
                AuthError::Network(_) => ErrorCode::Network,
                AuthError::Io(_) => ErrorCode::Daemon,
                _ => ErrorCode::Auth,
            }
        };
        let op = auth_message(code, &error);
        match error {
            AuthError::NeedsAuth(_) => op.with_hint(
                "log in again with `rail account login <name>` or the rail_account_login tool",
            ),
            _ if code == ErrorCode::ConsentRequired => op.with_hint(
                "an administrator grants admin consent (rail_admin_consent), then try again",
            ),
            _ => op,
        }
    }
}

fn auth_message(code: ErrorCode, error: &AuthError) -> OpError {
    let class = error.class();
    let fixed =
        |message: &str, detail: String| OpError::new(code, class, message).with_detail(detail);
    match error {
        AuthError::Denied(description) => {
            fixed("the provider refused the login", description.clone())
        }
        AuthError::Exchange(body) => fixed("the token exchange failed", body.clone()),
        AuthError::NeedsAuth(description) => fixed(
            "the provider rejected the refresh token",
            description.clone(),
        ),
        AuthError::Network(text) => fixed("could not reach the provider", text.clone()),
        AuthError::Config(text) => fixed("the OAuth configuration is invalid", text.clone()),
        AuthError::Io(e) => fixed("the loopback listener failed", e.to_string()),
        // Fixed text only: Rail wrote these messages.
        AuthError::MissingCode | AuthError::MissingConsent | AuthError::NoRefreshToken => {
            OpError::new(code, class, error.to_string())
        }
        AuthError::Secret(_) => OpError::internal("a secret store error reached the auth mapping"),
    }
}

/// A URL as Rail repeats it in a message: no user name, password, query or fragment.
fn shown_url(url: &str) -> String {
    match url::Url::parse(url) {
        Ok(mut parsed) => {
            let _ = parsed.set_username("");
            let _ = parsed.set_password(None);
            parsed.set_query(None);
            parsed.set_fragment(None);
            parsed.to_string()
        }
        Err(_) => url.to_string(),
    }
}

/// A preset failure as an operation reports it. A preset source may be any file the daemon can
/// read, so a parse failure names the path (or URL), the line and column, and a fixed reason,
/// never the parser's text, which can quote the file; transport text goes into `detail`.
impl From<PresetError> for OpError {
    fn from(error: PresetError) -> Self {
        let class = error.class();
        let op = |message: String| Self::new(ErrorCode::Preset, class, message);
        match &error {
            PresetError::Parse(config) => op(preset_file_message(config)),
            PresetError::NotHttps { url } => op(format!(
                "refusing to fetch the preset from {}: it must be an https:// URL, because a preset fetched over plain http could be altered by anyone on the network",
                shown_url(url)
            )),
            PresetError::BadUrl { url, message } => {
                op(format!("{:?} is not a valid URL", shown_url(url))).with_detail(message.clone())
            }
            PresetError::Network { url, message } => op(format!(
                "could not fetch the preset from {}",
                shown_url(url)
            ))
            .with_detail(message.clone()),
            PresetError::Status { url, status } => op(format!(
                "the preset server at {} answered HTTP {status}",
                shown_url(url)
            )),
            PresetError::TooLarge { url } => op(format!(
                "the preset at {} is larger than 64 KiB",
                shown_url(url)
            )),
        }
    }
}

/// `path:line:col: not a valid Rail preset`, and friends, from a preset's `ConfigError`.
pub fn preset_file_message(error: &ConfigError) -> String {
    const NOT_A_PRESET: &str = "not a valid Rail preset";
    match error {
        ConfigError::Parse {
            path, line, col, ..
        } => {
            format!("{}:{line}:{col}: {NOT_A_PRESET}", one_line(path))
        }
        ConfigError::Invalid { path, message } if message == PRESET_TOO_LARGE => {
            format!("{}: larger than 64 KiB; {NOT_A_PRESET}", one_line(path))
        }
        ConfigError::Invalid { path, .. } | ConfigError::Missing { path } => {
            format!("{}: {NOT_A_PRESET}", one_line(path))
        }
        ConfigError::Io { path, source } => {
            format!("{}: cannot be read ({})", one_line(path), source.kind())
        }
    }
}

/// A path on one line: newlines and control characters are escaped, so a hostile file name
/// cannot start a line of its own in a message.
fn one_line(path: &std::path::Path) -> String {
    path.display().to_string().escape_debug().to_string()
}
