//! Authorization-code flow with PKCE over a loopback redirect.

use oauth2::basic::{BasicErrorResponse, BasicErrorResponseType, BasicTokenResponse};
use oauth2::{
    AuthorizationCode, CsrfToken, PkceCodeChallenge, RequestTokenError, Scope, TokenResponse,
};
use url::Url;

use super::AuthError;
use super::client::{build_client, send_token_request};
use super::endpoints::OAuthEndpoints;
use super::loopback::Loopback;
use super::secrets::TokenSet;
use crate::config::AccountConfig;

const DEFAULT_EXPIRES_SECS: i64 = 3600;

/// Run the full login. `open_url` is called once with the consent URL; `now` is unix seconds.
pub async fn run_auth_code_flow(
    account: &AccountConfig,
    endpoints: &OAuthEndpoints,
    http: &reqwest::Client,
    open_url: impl FnOnce(&Url),
    now: i64,
) -> Result<TokenSet, AuthError> {
    let loopback = Loopback::bind().await?;
    let client = build_client(account, endpoints, Some(&loopback.redirect_url()))?;
    let (pkce_challenge, pkce_verifier) = PkceCodeChallenge::new_random_sha256();

    let mut request = client
        .authorize_url(CsrfToken::new_random)
        .set_pkce_challenge(pkce_challenge);
    for scope in &endpoints.scopes {
        request = request.add_scope(Scope::new(scope.clone()));
    }
    for (key, value) in &endpoints.extra_auth_params {
        request = request.add_extra_param(key.clone(), value.clone());
    }
    let (url, csrf) = request.url();

    tracing::info!(address = %account.address, "waiting for OAuth consent in the browser");
    open_url(&url);
    let code = loopback.wait_for_code(csrf.secret()).await?;

    let response = client
        .exchange_code(AuthorizationCode::new(code))
        .set_pkce_verifier(pkce_verifier)
        .request_async(&|req| send_token_request(http.clone(), req))
        .await
        .map_err(map_token_error)?;
    token_set_from_response(&response, None, now)
}

/// Build a `TokenSet`, keeping `previous_refresh` when the provider omits a new refresh token.
pub(crate) fn token_set_from_response(
    response: &BasicTokenResponse,
    previous_refresh: Option<&str>,
    now: i64,
) -> Result<TokenSet, AuthError> {
    let refresh_token = response
        .refresh_token()
        .map(|t| t.secret().clone())
        .or_else(|| previous_refresh.map(str::to_string))
        .ok_or(AuthError::NoRefreshToken)?;
    let expires_in = response.expires_in().map_or(DEFAULT_EXPIRES_SECS, |d| {
        i64::try_from(d.as_secs()).unwrap_or(DEFAULT_EXPIRES_SECS)
    });
    Ok(TokenSet {
        access_token: response.access_token().secret().clone(),
        refresh_token,
        expires_at: now + expires_in,
    })
}

/// OAuth error codes (RFC 6749 §5.2 extensions) that say "try again later".
const TRANSIENT_ERROR_CODES: &[&str] = &["server_error", "temporarily_unavailable"];

/// Classify a token endpoint failure. `invalid_grant` means the user must log in
/// again; 5xx, 429 and `server_error`/`temporarily_unavailable` are transient.
pub(crate) fn map_token_error<RE>(error: RequestTokenError<RE, BasicErrorResponse>) -> AuthError
where
    RE: std::error::Error + 'static,
{
    match error {
        RequestTokenError::ServerResponse(resp)
            if matches!(resp.error(), BasicErrorResponseType::InvalidGrant) =>
        {
            AuthError::NeedsAuth(resp.error_description().cloned().unwrap_or_default())
        }
        RequestTokenError::ServerResponse(resp)
            if TRANSIENT_ERROR_CODES.contains(&resp.error().as_ref()) =>
        {
            AuthError::Network(resp.to_string())
        }
        RequestTokenError::ServerResponse(resp) => AuthError::Exchange(resp.to_string()),
        RequestTokenError::Request(e) => AuthError::Network(e.to_string()),
        other => AuthError::Exchange(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::client::http_client;
    use crate::auth::endpoints::endpoints_for;
    use crate::config::{AccountConfig, AccountKind};
    use oauth2::AuthType;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn account() -> AccountConfig {
        AccountConfig {
            client_secret: Some("sec".into()),
            color: None,
            ..crate::test_support::sample_account()
        }
    }

    fn endpoints(server: &MockServer) -> OAuthEndpoints {
        let mut e = endpoints_for(AccountKind::Gmail, None);
        e.auth_url = format!("{}/auth", server.uri());
        e.token_url = format!("{}/token", server.uri());
        e.auth_type = AuthType::BasicAuth;
        e
    }

    fn query(url: &Url, key: &str) -> String {
        url.query_pairs()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.into_owned())
            .unwrap()
    }

    #[tokio::test]
    async fn flow_exchanges_code_with_pkce_and_returns_tokens() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code=the-code"))
            .and(body_string_contains("code_verifier="))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "A1", "refresh_token": "R1", "expires_in": 3600, "token_type": "Bearer"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let (tx, rx) = tokio::sync::oneshot::channel::<Url>();
        let browser = tokio::spawn(async move {
            let url = rx.await.unwrap();
            assert_eq!(query(&url, "access_type"), "offline");
            assert_eq!(query(&url, "prompt"), "consent");
            assert!(url.query_pairs().any(|(k, _)| k == "code_challenge"));
            let state = query(&url, "state");
            let redirect = query(&url, "redirect_uri");
            reqwest::get(format!("{redirect}/?code=the-code&state={state}"))
                .await
                .unwrap();
        });

        let tokens = run_auth_code_flow(
            &account(),
            &endpoints(&server),
            &http_client().unwrap(),
            |u| tx.send(u.clone()).unwrap(),
            1_000,
        )
        .await
        .unwrap();
        browser.await.unwrap();
        assert_eq!(
            tokens,
            TokenSet {
                access_token: "A1".into(),
                refresh_token: "R1".into(),
                expires_at: 4_600
            }
        );
    }

    #[tokio::test]
    async fn flow_reports_denied_consent() {
        let server = MockServer::start().await;
        let (tx, rx) = tokio::sync::oneshot::channel::<Url>();
        tokio::spawn(async move {
            let url = rx.await.unwrap();
            let state = query(&url, "state");
            let redirect = query(&url, "redirect_uri");
            reqwest::get(format!(
                "{redirect}/?error=access_denied&error_description=User%20said%20no&state={state}"
            ))
            .await
            .unwrap();
        });
        let err = run_auth_code_flow(
            &account(),
            &endpoints(&server),
            &http_client().unwrap(),
            |u| tx.send(u.clone()).unwrap(),
            0,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, AuthError::Denied(ref m) if m == "User said no"),
            "{err:?}"
        );
        assert_eq!(err.class(), crate::error::ErrorClass::Permanent);
    }

    #[tokio::test]
    async fn flow_fails_when_provider_omits_refresh_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "A1", "expires_in": 3600, "token_type": "Bearer"
            })))
            .mount(&server)
            .await;
        let (tx, rx) = tokio::sync::oneshot::channel::<Url>();
        tokio::spawn(async move {
            let url = rx.await.unwrap();
            let state = query(&url, "state");
            let redirect = query(&url, "redirect_uri");
            reqwest::get(format!("{redirect}/?code=x&state={state}"))
                .await
                .unwrap();
        });
        let err = run_auth_code_flow(
            &account(),
            &endpoints(&server),
            &http_client().unwrap(),
            |u| tx.send(u.clone()).unwrap(),
            0,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AuthError::NoRefreshToken), "{err:?}");
    }

    #[tokio::test]
    async fn code_exchange_503_is_transient() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/token"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;
        let (tx, rx) = tokio::sync::oneshot::channel::<Url>();
        tokio::spawn(async move {
            let url = rx.await.unwrap();
            let state = query(&url, "state");
            let redirect = query(&url, "redirect_uri");
            reqwest::get(format!("{redirect}/?code=x&state={state}"))
                .await
                .unwrap();
        });
        let err = run_auth_code_flow(
            &account(),
            &endpoints(&server),
            &http_client().unwrap(),
            |u| tx.send(u.clone()).unwrap(),
            0,
        )
        .await
        .unwrap_err();
        assert!(
            matches!(err, AuthError::Network(ref m) if m.contains("503")),
            "{err:?}"
        );
        assert_eq!(err.class(), crate::error::ErrorClass::Transient);
    }
}
