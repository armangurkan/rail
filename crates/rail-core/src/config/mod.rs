//! User configuration: `config.toml`.

pub mod edit;
pub mod load;
pub mod model;

pub use edit::{append_account, default_account_name, remove_account, set_secrets};
pub use load::{ConfigError, MAX_ADDRESS_CHARS, line_col, load, load_text_or_empty, parse};
pub use model::{
    AccountConfig, AccountKind, AiSend, Config, DAY_SECS, General, Icons, Layout, OnePasswordConfig,
    SecretsBackend, SyncSettings, is_format_char, is_plain, not_plain,
};
/// A config value with its byte range in the file (for line numbers).
pub use toml::Spanned;
