//! Authorization-code flow with PKCE over a loopback redirect.

use oauth2::basic::{BasicErrorResponse, BasicErrorResponseType, BasicTokenResponse};
use oauth2::{
    AuthorizationCode, CsrfToken, PkceCodeChallenge, PkceCodeVerifier, RequestTokenError, Scope,
    TokenResponse,
};
use url::Url;

use super::AuthError;
use super::client::{OAuthClient, build_client, send_token_request};
use super::endpoints::{OAuthEndpoints, admin_consent_url, is_concrete_tenant};
use super::loopback::Loopback;
use super::secrets::TokenSet;
use crate::config::AccountConfig;

/// A fresh unguessable token (16 random bytes from the OS, base64url), the same kind the OAuth
/// `state` parameter uses.
pub fn random_token() -> String {
    CsrfToken::new_random().secret().clone()
}

const DEFAULT_EXPIRES_SECS: i64 = 3600;

/// A login whose URL is ready: the loopback listener is bound and the PKCE verifier and CSRF
/// state are held until the redirect arrives. No `Debug`: it holds the verifier.
pub struct AuthSession {
    loopback: Loopback,
    client: OAuthClient,
    verifier: PkceCodeVerifier,
    csrf: CsrfToken,
    url: Url,
}

/// Bind the loopback listener and build the authorization URL, without opening anything.
pub async fn begin_auth_code(
    account: &AccountConfig,
    endpoints: &OAuthEndpoints,
) -> Result<AuthSession, AuthError> {
    let loopback = Loopback::bind().await?;
    let client = build_client(account, endpoints, Some(&loopback.redirect_url()))?;
    let (pkce_challenge, verifier) = PkceCodeChallenge::new_random_sha256();
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
    tracing::info!(address = %account.address, "login URL ready");
    Ok(AuthSession {
        loopback,
        client,
        verifier,
        csrf,
        url,
    })
}

impl AuthSession {
    /// The page the user opens. It carries the state and the PKCE challenge, never a secret.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Wait for the redirect and exchange its code. `now` (unix seconds) is read when the
    /// tokens arrive, for their expiry.
    pub async fn finish(
        self,
        http: &reqwest::Client,
        now: impl FnOnce() -> i64,
    ) -> Result<TokenSet, AuthError> {
        let code = self.loopback.wait_for_code(self.csrf.secret()).await?;
        let response = self
            .client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(self.verifier)
            .request_async(&|req| send_token_request(http.clone(), req))
            .await
            .map_err(map_token_error)?;
        token_set_from_response(&response, None, now())
    }
}

/// Run the full login. `open_url` is called once with the consent URL; `now` is unix seconds.
pub async fn run_auth_code_flow(
    account: &AccountConfig,
    endpoints: &OAuthEndpoints,
    http: &reqwest::Client,
    open_url: impl FnOnce(&Url),
    now: i64,
) -> Result<TokenSet, AuthError> {
    let session = begin_auth_code(account, endpoints).await?;
    open_url(session.url());
    session.finish(http, || now).await
}

/// An admin consent whose URL is ready.
pub struct ConsentSession {
    loopback: Loopback,
    state: CsrfToken,
    url: Url,
}

/// Bind the loopback listener and build Microsoft's admin-consent URL for `tenant`.
///
/// `tenant` must name one organisation: `common`, `organizations`, `consumers` and the empty
/// string are refused with `AuthError::Config` before anything is bound.
pub async fn begin_admin_consent(
    client_id: &str,
    tenant: &str,
) -> Result<ConsentSession, AuthError> {
    if !is_concrete_tenant(tenant) {
        return Err(AuthError::Config(
            "admin consent needs the organisation's tenant id or domain, not a shared alias"
                .to_string(),
        ));
    }
    let loopback = Loopback::bind().await?;
    let state = CsrfToken::new_random();
    let url = admin_consent_url(client_id, tenant, &loopback.redirect_url(), state.secret());
    tracing::info!(tenant, "admin consent URL ready");
    Ok(ConsentSession {
        loopback,
        state,
        url,
    })
}

impl ConsentSession {
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Wait for the redirect; the result is the tenant that consented (empty if Microsoft
    /// omitted it).
    pub async fn finish(self) -> Result<String, AuthError> {
        self.loopback
            .wait_for_admin_consent(self.state.secret())
            .await
    }
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

    /// The admin consent the daemon drives through `begin_admin_consent` and
    /// `ConsentSession::finish`, with `open_url` standing in for the browser.
    async fn run_admin_consent(
        client_id: &str,
        tenant: &str,
        open_url: impl FnOnce(&Url),
    ) -> Result<String, AuthError> {
        let session = begin_admin_consent(client_id, tenant).await?;
        open_url(session.url());
        session.finish().await
    }

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

    #[tokio::test]
    async fn admin_consent_returns_the_tenant_from_the_redirect() {
        let (tx, rx) = tokio::sync::oneshot::channel::<Url>();
        let browser = tokio::spawn(async move {
            let url = rx.await.unwrap();
            assert_eq!(url.path(), "/contoso.onmicrosoft.com/v2.0/adminconsent");
            assert_eq!(query(&url, "client_id"), "cid");
            assert_eq!(query(&url, "scope"), "https://graph.microsoft.com/.default");
            let state = query(&url, "state");
            let redirect = query(&url, "redirect_uri");
            assert!(redirect.starts_with("http://127.0.0.1:"), "{redirect}");
            reqwest::get(format!(
                "{redirect}/?admin_consent=True&tenant=tid-9&state={state}"
            ))
            .await
            .unwrap();
        });
        let tenant = run_admin_consent("cid", "contoso.onmicrosoft.com", |u| {
            tx.send(u.clone()).unwrap();
        })
        .await
        .unwrap();
        browser.await.unwrap();
        assert_eq!(tenant, "tid-9");
    }

    #[tokio::test]
    async fn admin_consent_reports_a_refusal() {
        let (tx, rx) = tokio::sync::oneshot::channel::<Url>();
        tokio::spawn(async move {
            let url = rx.await.unwrap();
            let state = query(&url, "state");
            let redirect = query(&url, "redirect_uri");
            reqwest::get(format!(
                "{redirect}/?error=access_denied&error_description=AADSTS65004&state={state}"
            ))
            .await
            .unwrap();
        });
        let err = run_admin_consent("cid", "contoso.onmicrosoft.com", |u| {
            tx.send(u.clone()).unwrap();
        })
        .await
        .unwrap_err();
        assert!(
            matches!(err, AuthError::Denied(ref m) if m == "AADSTS65004"),
            "{err:?}"
        );
        assert_eq!(err.class(), crate::error::ErrorClass::Permanent);
    }

    #[tokio::test]
    async fn admin_consent_refuses_a_non_concrete_tenant_without_opening_a_browser() {
        for tenant in ["common", "organizations", "consumers", ""] {
            let err = run_admin_consent("cid", tenant, |_| panic!("must not open a URL"))
                .await
                .unwrap_err();
            assert!(matches!(err, AuthError::Config(_)), "{tenant}: {err:?}");
            assert_eq!(err.class(), crate::error::ErrorClass::Permanent);
        }
    }
}
