use oauth2::basic::BasicClient;
use oauth2::{AuthUrl, ClientId, ClientSecret, EndpointNotSet, EndpointSet, RedirectUrl, TokenUrl};

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

/// HTTP client for token requests. Redirects are disabled on purpose (SSRF hardening).
pub fn http_client() -> Result<reqwest::Client, AuthError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| AuthError::Config(format!("http client: {e}")))
}
