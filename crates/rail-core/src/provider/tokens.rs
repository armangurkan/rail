//! Access tokens for API requests: the existing OAuth `TokenSource` (refreshing through the
//! stored refresh token and the secret store), behind a trait the HTTP layer and tests share.
//! Tokens never leave this module except as the bearer header.

use crate::auth::{AuthError, TokenSource};
use crate::error::ErrorClass;

use super::ProviderError;

#[async_trait::async_trait]
pub trait AccessTokens: Send + Sync {
    /// A token valid for at least a minute.
    async fn token(&self) -> Result<String, ProviderError>;
    /// A new token whatever the cached one's expiry says: the API refused it.
    async fn refresh(&self) -> Result<String, ProviderError>;
}

pub struct OAuthTokens {
    source: TokenSource,
}

impl OAuthTokens {
    pub fn new(source: TokenSource) -> Self {
        Self { source }
    }
}

#[async_trait::async_trait]
impl AccessTokens for OAuthTokens {
    async fn token(&self) -> Result<String, ProviderError> {
        self.source.access_token().await.map_err(from_auth)
    }

    async fn refresh(&self) -> Result<String, ProviderError> {
        self.source.force_refresh().await.map_err(from_auth)
    }
}

/// `invalid_grant` (and no stored tokens) is `NeedsAuth`; the token endpoint's and the token
/// store's own text is detail.
pub(crate) fn from_auth(error: AuthError) -> ProviderError {
    match error {
        AuthError::NeedsAuth(text) => ProviderError::NeedsAuth { detail: Some(text) },
        AuthError::Network(text) => {
            ProviderError::transient("could not reach the token endpoint").with_detail(text)
        }
        // A keyring or 1Password hiccup may clear up; the store's own class says which.
        AuthError::Secret(secret) => {
            classed(secret.class(), "the token store failed", secret.to_string())
        }
        other => classed(
            other.class(),
            "the token request or the token store failed",
            other.to_string(),
        ),
    }
}

fn classed(class: ErrorClass, fixed: &str, detail: String) -> ProviderError {
    match class {
        ErrorClass::Transient => ProviderError::transient(fixed).with_detail(detail),
        ErrorClass::Permanent => ProviderError::permanent(fixed).with_detail(detail),
    }
}

/// Tokens for tests: hands out `tokens` in order, moving to the next on each refresh.
#[cfg(test)]
pub(crate) struct FixedTokens {
    tokens: Vec<String>,
    current: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl FixedTokens {
    pub(crate) fn new<const N: usize>(tokens: [&str; N]) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            tokens: tokens.iter().map(|t| (*t).to_string()).collect(),
            current: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    pub(crate) fn refreshes(&self) -> usize {
        self.current.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn at(&self, index: usize) -> String {
        self.tokens[index.min(self.tokens.len() - 1)].clone()
    }
}

#[cfg(test)]
#[async_trait::async_trait]
impl AccessTokens for FixedTokens {
    async fn token(&self) -> Result<String, ProviderError> {
        Ok(self.at(self.refreshes()))
    }

    async fn refresh(&self) -> Result<String, ProviderError> {
        let next = self
            .current
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        Ok(self.at(next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::SecretError;

    /// T3-C(b): a keyring or 1Password hiccup stays transient; a corrupt store does not.
    #[test]
    fn secret_store_hiccups_stay_transient() {
        let transient = [
            SecretError::Keyring("busy".into()),
            SecretError::Io {
                path: "secrets.json".into(),
                source: std::io::Error::other("disk"),
            },
            SecretError::OnePasswordTimeout,
            SecretError::OnePasswordIo {
                what: "read from",
                message: "pipe".into(),
            },
        ];
        for secret in transient {
            let error = from_auth(AuthError::Secret(secret));
            assert_eq!(error.class(), ErrorClass::Transient, "{error:?}");
        }
        let permanent = [
            SecretError::Encoding("bad".into()),
            SecretError::OnePasswordLocked("locked".into()),
        ];
        for secret in permanent {
            let error = from_auth(AuthError::Secret(secret));
            assert_eq!(error.class(), ErrorClass::Permanent, "{error:?}");
        }
    }

    #[test]
    fn invalid_grant_needs_auth_and_network_is_transient() {
        let auth = from_auth(AuthError::NeedsAuth("revoked".into()));
        assert!(matches!(auth, ProviderError::NeedsAuth { .. }));
        let net = from_auth(AuthError::Network("down".into()));
        assert_eq!(net.class(), ErrorClass::Transient);
        assert_eq!(net.detail(), Some("down"));
    }
}
