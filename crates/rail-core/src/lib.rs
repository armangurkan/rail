//! Rail core: everything that is not the terminal UI.

pub mod auth;
pub mod config;
pub mod error;
pub mod onboarding;
pub mod paths;
pub mod store;

#[cfg(test)]
pub(crate) mod test_support;
