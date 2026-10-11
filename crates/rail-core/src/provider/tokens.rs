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
        other => {
            let text = other.to_string();
            let fixed = "the token request or the token store failed";
            match other.class() {
                ErrorClass::Transient => ProviderError::transient(fixed).with_detail(text),
                ErrorClass::Permanent => ProviderError::permanent(fixed).with_detail(text),
            }
        }
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
