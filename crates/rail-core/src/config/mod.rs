//! User configuration: `config.toml`.

pub mod edit;
pub mod load;
pub mod model;

pub use edit::{append_account, default_account_name};
pub use load::{ConfigError, load, parse};
pub use model::{AccountConfig, AccountKind, AiSend, Config, General, Layout, SecretsBackend};
