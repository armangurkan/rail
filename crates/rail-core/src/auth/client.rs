use std::time::Duration;

use oauth2::basic::BasicClient;
use oauth2::http::StatusCode;
use oauth2::{
    AsyncHttpClient, AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet, HttpClientError,
    HttpRequest, HttpResponse, RedirectUrl, TokenUrl,
};

use super::AuthError;
use super::endpoints::OAuthEndpoints;
use crate::config::AccountConfig;

/// An oauth2 client with the authorization and token endpoints set.
pub type OAuthClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// Build an oauth2 client for an account; `redirect` is the loopback URL during login.
pub fn build_client(
    account: &AccountConfig,
    endpoints: &OAuthEndpoints,
    redirect: Option<&str>,
) -> Result<OAuthClient, AuthError> {
    let bad = |what: &str, e: url::ParseError| AuthError::Config(format!("{what}: {e}"));
    let mut client = BasicClient::new(ClientId::new(account.client_id.clone()))
        .set_auth_uri(AuthUrl::new(endpoints.auth_url.clone()).map_err(|e| bad("auth url", e))?)
        .set_token_uri(TokenUrl::new(endpoints.token_url.clone()).map_err(|e| bad("token url", e))?)
        .set_auth_type(endpoints.auth_type.clone());
    if let Some(secret) = &account.client_secret {
        client = client.set_client_secret(ClientSecret::new(secret.clone()));
    }
    if let Some(redirect) = redirect {
        client = client.set_redirect_uri(
            RedirectUrl::new(redirect.to_string()).map_err(|e| bad("redirect url", e))?,
        );
    }
    Ok(client)
}

/// How long to wait for a TCP/TLS connection to the token endpoint.
const TOKEN_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Upper bound on a whole token request, so a stalled endpoint cannot hang a
/// login or hold a `TokenSource` refresh lock forever.
const TOKEN_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// HTTP client for token requests. Redirects are disabled on purpose (SSRF hardening).
pub fn http_client() -> Result<reqwest::Client, AuthError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(TOKEN_CONNECT_TIMEOUT)
        .timeout(TOKEN_REQUEST_TIMEOUT)
        .build()
        .map_err(|e| AuthError::Config(format!("http client: {e}")))
}

/// Failure of one token-endpoint HTTP exchange, before oauth2 parses the body.
#[derive(Debug, thiserror::Error)]
pub enum TokenHttpError {
    #[error(transparent)]
    Client(#[from] HttpClientError<reqwest::Error>),
    /// 5xx or 429: the endpoint is down or throttling, so a retry may succeed.
    #[error("token endpoint returned HTTP {0}")]
    Unavailable(StatusCode),
}

/// Send a token request, turning 5xx and 429 responses into `TokenHttpError::Unavailable`
/// instead of letting oauth2 misread their bodies as a permanent protocol error.
/// Pass it to oauth2 as `&|req| send_token_request(http.clone(), req)`: the client
/// is taken by value (an `Arc` clone) so the future borrows nothing and stays
/// `Send` for every caller lifetime.
pub(crate) async fn send_token_request(
    http: reqwest::Client,
    request: HttpRequest,
) -> Result<HttpResponse, TokenHttpError> {
    let response = http.call(request).await?;
    let status = response.status();
    if status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS {
        return Err(TokenHttpError::Unavailable(status));
    }
    Ok(response)
}
