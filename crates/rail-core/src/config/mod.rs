//! User configuration: `config.toml`.

pub mod edit;
pub mod load;
pub mod model;

pub use edit::{append_account, default_account_name, remove_account, set_secrets};
pub use load::{ConfigError, load, load_text_or_empty, parse};
pub use model::{
    AccountConfig, AccountKind, AiSend, Config, General, Layout, OnePasswordConfig, SecretsBackend,
};
