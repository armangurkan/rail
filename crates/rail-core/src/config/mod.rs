//! User configuration: `config.toml`.

pub mod load;
pub mod model;

pub use load::{ConfigError, load, parse};
pub use model::{AccountConfig, AccountKind, Config, General, Layout, SecretsBackend};
