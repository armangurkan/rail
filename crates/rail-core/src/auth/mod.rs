//! OAuth2 login, token storage and refresh.

pub mod client;
pub mod endpoints;
pub mod flow;
pub mod loopback;
pub mod secrets;

pub use client::{OAuthClient, build_client, http_client};
pub use endpoints::{GMAIL_SCOPE, OAuthEndpoints, endpoints_for};
pub use flow::run_auth_code_flow;
pub use secrets::{
    FileSecretStore, KeyringSecretStore, MemorySecretStore, SecretError, SecretStore, TokenSet,
    open_secret_store,
};

use crate::error::ErrorClass;

/// Errors from the OAuth login and token refresh flows.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("authorization denied by the provider: {0}")]
    Denied(String),
    #[error("the OAuth redirect did not include an authorization code")]
    MissingCode,
    #[error(
        "the provider returned no refresh token. For Google: set the OAuth app to \"In production\" and revoke Rail at https://myaccount.google.com/permissions, then try again"
    )]
    NoRefreshToken,
    #[error("token exchange failed: {0}")]
    Exchange(String),
    #[error("refresh token rejected ({0}); run `rail account add` again to re-authenticate")]
    NeedsAuth(String),
    #[error("network error talking to the token endpoint: {0}")]
    Network(String),
    #[error("invalid OAuth configuration: {0}")]
    Config(String),
    #[error(transparent)]
    Secret(#[from] SecretError),
    #[error("loopback listener: {0}")]
    Io(#[from] std::io::Error),
}

impl AuthError {
    /// Whether retrying later could succeed.
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Network(_) | Self::Io(_) => ErrorClass::Transient,
            _ => ErrorClass::Permanent,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn class_covers_every_variant() {
        let transient = [
            AuthError::Network("x".into()),
            AuthError::Io(std::io::Error::other("x")),
        ];
        let permanent = [
            AuthError::Denied("x".into()),
            AuthError::MissingCode,
            AuthError::NoRefreshToken,
            AuthError::Exchange("x".into()),
            AuthError::NeedsAuth("x".into()),
            AuthError::Config("x".into()),
            AuthError::Secret(SecretError::Keyring("x".into())),
        ];
        assert!(transient.iter().all(|e| e.class() == ErrorClass::Transient));
        assert!(permanent.iter().all(|e| e.class() == ErrorClass::Permanent));
    }
}
