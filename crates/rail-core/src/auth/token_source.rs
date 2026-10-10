//! Hands out a valid access token, refreshing it when needed. One refresh at a time per account.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use oauth2::RefreshToken;
use tokio::sync::Mutex;

use super::AuthError;
use super::client::{OAuthClient, send_token_request};
use super::flow::{map_token_error, token_set_from_response};
use super::secrets::{SecretStore, TokenSet};

/// Refresh when fewer than this many seconds remain.
const REFRESH_MARGIN_SECS: i64 = 60;

/// Caches an account's tokens and refreshes them on demand, one refresh at a time.
pub struct TokenSource {
    address: String,
    client: OAuthClient,
    http: reqwest::Client,
    secrets: Arc<dyn SecretStore>,
    cached: Mutex<Option<TokenSet>>,
    clock: Box<dyn Fn() -> i64 + Send + Sync>,
}

/// Current Unix time in seconds.
pub fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl TokenSource {
    /// A token source for the account with this address, backed by `secrets`.
    pub fn new(
        address: impl Into<String>,
        client: OAuthClient,
        http: reqwest::Client,
        secrets: Arc<dyn SecretStore>,
    ) -> Self {
        Self {
            address: address.into(),
            client,
            http,
            secrets,
            cached: Mutex::new(None),
            clock: Box::new(unix_now),
        }
    }

    /// Replace the clock (Unix seconds); for tests.
    pub fn with_clock(mut self, clock: impl Fn() -> i64 + Send + Sync + 'static) -> Self {
        self.clock = Box::new(clock);
        self
    }

    /// The account address this source serves.
    pub fn address(&self) -> &str {
        &self.address
    }

    /// A bearer token valid for at least `REFRESH_MARGIN_SECS`.
    pub async fn access_token(&self) -> Result<String, AuthError> {
        // Holding the lock across the refresh is the single-flight guarantee.
        let mut cached = self.cached.lock().await;
        let now = (self.clock)();
        if let Some(tokens) = cached.as_ref().filter(|t| fresh(t, now)) {
            return Ok(tokens.access_token.clone());
        }
        let current = match cached.take() {
            Some(tokens) => tokens,
            None => self
                .secrets
                .get_tokens(&self.address)?
                .ok_or_else(|| AuthError::NeedsAuth("no stored tokens".to_string()))?,
        };
        let tokens = if fresh(&current, now) {
            current
        } else {
            self.refresh(&current, now).await?
        };
        let access = tokens.access_token.clone();
        *cached = Some(tokens);
        Ok(access)
    }

    async fn refresh(&self, current: &TokenSet, now: i64) -> Result<TokenSet, AuthError> {
        tracing::debug!(address = %self.address, "refreshing access token");
        let response = self
            .client
            .exchange_refresh_token(&RefreshToken::new(current.refresh_token.clone()))
            .request_async(&|req| send_token_request(self.http.clone(), req))
            .await
            .map_err(map_token_error)?;
        let tokens = token_set_from_response(&response, Some(&current.refresh_token), now)?;
        self.secrets.set_tokens(&self.address, &tokens)?;
        Ok(tokens)
    }
}

fn fresh(tokens: &TokenSet, now: i64) -> bool {
    tokens.expires_at - now > REFRESH_MARGIN_SECS
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::client::{build_client, http_client};
    use crate::auth::endpoints::endpoints_for;
    use crate::auth::secrets::MemorySecretStore;
    use crate::config::{AccountConfig, AccountKind};
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const ADDRESS: &str = "me@example.com";

    fn source(server_uri: &str, secrets: Arc<dyn SecretStore>, now: i64) -> TokenSource {
        let account = AccountConfig {
            name: "w".into(),
            address: ADDRESS.into(),
            color: None,
            ..crate::test_support::sample_account()
        };
        let mut endpoints = endpoints_for(AccountKind::Gmail, None);
        endpoints.token_url = format!("{server_uri}/token");
        let client = build_client(&account, &endpoints, None).unwrap();
        TokenSource::new(ADDRESS, client, http_client().unwrap(), secrets).with_clock(move || now)
    }

    fn stored(expires_at: i64) -> Arc<dyn SecretStore> {
        let s = MemorySecretStore::default();
        s.set_tokens(
            ADDRESS,
            &TokenSet {
                access_token: "old-access".into(),
                refresh_token: "old-refresh".into(),
                expires_at,
            },
        )
        .unwrap();
        Arc::new(s)
    }

    #[tokio::test]
    async fn fresh_token_is_returned_without_a_request() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500))
            .expect(0)
            .mount(&server)
            .await;
        let src = source(&server.uri(), stored(10_000), 1_000);
        assert_eq!(src.access_token().await.unwrap(), "old-access");
        assert_eq!(src.access_token().await.unwrap(), "old-access");
    }

    #[tokio::test]
    async fn token_inside_refresh_margin_is_refreshed() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("refresh_token=old-refresh"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "new-access", "refresh_token": "new-refresh", "expires_in": 3600, "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&server)
            .await;
        let secrets = stored(1_030); // expires in 30s, margin is 60s
        let src = source(&server.uri(), secrets.clone(), 1_000);
        assert_eq!(src.access_token().await.unwrap(), "new-access");
        let saved = secrets.get_tokens(ADDRESS).unwrap().unwrap();
        assert_eq!(saved.refresh_token, "new-refresh");
        assert_eq!(saved.expires_at, 4_600);
    }

    #[tokio::test]
    async fn refresh_keeps_old_refresh_token_when_omitted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "new-access", "expires_in": 3600, "token_type": "Bearer"
            })))
            .mount(&server)
            .await;
        let secrets = stored(0);
        let src = source(&server.uri(), secrets.clone(), 1_000);
        assert_eq!(src.access_token().await.unwrap(), "new-access");
        assert_eq!(
            secrets.get_tokens(ADDRESS).unwrap().unwrap().refresh_token,
            "old-refresh"
        );
    }

    #[tokio::test]
    async fn invalid_grant_means_needs_auth() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_json(serde_json::json!({
                "error": "invalid_grant", "error_description": "Token has been expired or revoked."
            })))
            .mount(&server)
            .await;
        let src = source(&server.uri(), stored(0), 1_000);
        let err = src.access_token().await.unwrap_err();
        assert!(
            matches!(err, AuthError::NeedsAuth(ref m) if m.contains("revoked")),
            "{err:?}"
        );
        assert_eq!(err.class(), crate::error::ErrorClass::Permanent);
    }

    async fn refresh_error(response: ResponseTemplate) -> AuthError {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(response)
            .mount(&server)
            .await;
        let src = source(&server.uri(), stored(0), 1_000);
        src.access_token().await.unwrap_err()
    }

    #[tokio::test]
    async fn token_endpoint_503_with_html_body_is_transient() {
        let err = refresh_error(
            ResponseTemplate::new(503)
                .set_body_raw("<html>Service Unavailable</html>", "text/html"),
        )
        .await;
        assert!(
            matches!(err, AuthError::Network(ref m) if m.contains("503")),
            "{err:?}"
        );
        assert_eq!(err.class(), crate::error::ErrorClass::Transient);
    }

    #[tokio::test]
    async fn token_endpoint_503_with_empty_body_is_transient() {
        let err = refresh_error(ResponseTemplate::new(503)).await;
        assert!(
            matches!(err, AuthError::Network(ref m) if m.contains("503")),
            "{err:?}"
        );
        assert_eq!(err.class(), crate::error::ErrorClass::Transient);
    }

    #[tokio::test]
    async fn token_endpoint_429_is_transient() {
        let err = refresh_error(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "error": "rate_limited"
        })))
        .await;
        assert!(
            matches!(err, AuthError::Network(ref m) if m.contains("429")),
            "{err:?}"
        );
        assert_eq!(err.class(), crate::error::ErrorClass::Transient);
    }

    #[tokio::test]
    async fn temporarily_unavailable_error_response_is_transient() {
        for code in ["temporarily_unavailable", "server_error"] {
            let err = refresh_error(
                ResponseTemplate::new(400).set_body_json(serde_json::json!({ "error": code })),
            )
            .await;
            assert_eq!(err.class(), crate::error::ErrorClass::Transient, "{err:?}");
        }
    }

    #[tokio::test]
    async fn other_4xx_responses_stay_permanent() {
        let err = refresh_error(
            ResponseTemplate::new(401)
                .set_body_json(serde_json::json!({ "error": "invalid_client" })),
        )
        .await;
        assert!(matches!(err, AuthError::Exchange(_)), "{err:?}");
        assert_eq!(err.class(), crate::error::ErrorClass::Permanent);
        let err =
            refresh_error(ResponseTemplate::new(404).set_body_raw("<html/>", "text/html")).await;
        assert_eq!(err.class(), crate::error::ErrorClass::Permanent, "{err:?}");
    }

    #[tokio::test]
    async fn unreachable_endpoint_is_transient() {
        let src = source("http://127.0.0.1:9", stored(0), 1_000);
        let err = src.access_token().await.unwrap_err();
        assert!(matches!(err, AuthError::Network(_)), "{err:?}");
        assert_eq!(err.class(), crate::error::ErrorClass::Transient);
    }

    #[tokio::test]
    async fn no_stored_tokens_means_needs_auth() {
        let server = MockServer::start().await;
        let src = source(&server.uri(), Arc::new(MemorySecretStore::default()), 1_000);
        assert!(matches!(
            src.access_token().await.unwrap_err(),
            AuthError::NeedsAuth(_)
        ));
    }

    #[tokio::test]
    async fn concurrent_callers_share_one_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_delay(std::time::Duration::from_millis(50))
                    .set_body_json(serde_json::json!({
                        "access_token": "new-access", "refresh_token": "r", "expires_in": 3600, "token_type": "Bearer"
                    })),
            )
            .expect(1)
            .mount(&server)
            .await;
        let src = Arc::new(source(&server.uri(), stored(0), 1_000));
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let src = src.clone();
                tokio::spawn(async move { src.access_token().await.unwrap() })
            })
            .collect();
        for task in tasks {
            assert_eq!(task.await.unwrap(), "new-access");
        }
    }
}
