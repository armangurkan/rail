//! User configuration: `config.toml`.

pub mod edit;
pub mod load;
pub mod model;

pub use edit::{
    append_account, default_account_name, remove_account, set_general, set_general_bool,
    set_general_number, set_general_text, set_secrets,
};
pub use load::{ConfigError, MAX_ADDRESS_CHARS, line_col, load, load_text_or_empty, parse};
pub use model::{
    AccentSpec, AccountConfig, AccountKind, AiSend, Config, DAY_SECS, FolderModel, General, Icons,
    Layout, OnePasswordConfig, SecretsBackend, SyncSettings, ThreadOrder, TimeFormat,
    is_format_char, is_plain, not_plain,
};
/// A config value with its byte range in the file (for line numbers).
pub use toml::Spanned;
