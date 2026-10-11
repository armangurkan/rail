//! Rail core: everything that is not the terminal UI.

pub mod accounts;
pub mod auth;
pub mod config;
pub mod daemon;
pub mod doctor;
pub mod error;
mod guarded;
pub mod guide;
pub mod onboarding;
pub mod ops;
pub mod paths;
pub mod provider;
pub mod store;
pub mod sync;

#[cfg(test)]
pub(crate) mod test_support;
