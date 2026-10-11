//! Building an account's provider: the default factory the daemon's sync manager uses.

use std::sync::Arc;

use url::Url;

use super::manager::ProviderRequest;
use crate::accounts::{endpoints_for_login, login_secret};
use crate::auth::{TokenSource, build_client, http_client, open_secret_store};
use crate::config::{AccountConfig, AccountKind};
use crate::ops::OpError;
use crate::provider::Provider;
use crate::provider::gmail::{GMAIL_API, GmailProvider};
use crate::provider::http::{ApiClient, api_http_client};
use crate::provider::tokens::OAuthTokens;

/// A Gmail provider for the account, refreshing its tokens through the configured secret
/// store. The client secret (an `op://` reference is read now) stays in the OAuth client.
pub fn gmail_for(request: ProviderRequest) -> Result<Arc<dyn Provider>, OpError> {
    if request.account.kind != AccountKind::Gmail {
        return Err(OpError::invalid_input(
            "only Gmail accounts sync in this version",
        ));
    }
    let secret = login_secret(&request.account, &request.config, &request.paths)?;
    let account = AccountConfig {
        client_secret: secret.map(|s| s.expose().to_string()),
        ..request.account.clone()
    };
    let endpoints = endpoints_for_login(&account, request.token_url.as_deref());
    let client = build_client(&account, &endpoints, None)?;
    let secrets = open_secret_store(&request.config, &request.paths, &request.ids);
    let source = TokenSource::new(account.address.clone(), client, http_client()?, secrets);
    let root = request.gmail_url.as_deref().unwrap_or(GMAIL_API);
    let base =
        Url::parse(root).map_err(|_| OpError::internal("the Gmail API address does not parse"))?;
    let api = ApiClient::new(api_http_client()?, base, Arc::new(OAuthTokens::new(source)))
        .with_observer(request.observer);
    Ok(Arc::new(GmailProvider::new(api)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::ItemIdCache;
    use crate::config::{Config, General, SecretsBackend};
    use crate::paths::Paths;
    use crate::test_support::sample_account;

    fn request(account: AccountConfig, paths: Paths) -> ProviderRequest {
        ProviderRequest {
            paths,
            config: Config {
                general: General {
                    secrets: SecretsBackend::File,
                    ..General::default()
                },
                ..Config::default()
            },
            account,
            ids: ItemIdCache::default(),
            token_url: Some("http://127.0.0.1:9/token".into()),
            gmail_url: Some("http://127.0.0.1:9/gmail/v1/users/me/".into()),
            observer: Arc::new(|_| {}),
        }
    }

    #[test]
    fn a_microsoft_account_gets_no_provider() {
        let tmp = tempfile::tempdir().unwrap();
        let account = AccountConfig {
            kind: AccountKind::M365,
            ..sample_account()
        };
        let err = gmail_for(request(account, Paths::all_in(tmp.path())))
            .err()
            .expect("refused");
        assert_eq!(err.code, crate::ops::ErrorCode::InvalidInput);
    }

    #[test]
    fn a_gmail_account_builds_without_touching_the_network() {
        let tmp = tempfile::tempdir().unwrap();
        let built = gmail_for(request(sample_account(), Paths::all_in(tmp.path())));
        assert!(built.is_ok());
        let mut bad = request(sample_account(), Paths::all_in(tmp.path()));
        bad.gmail_url = Some("not a url".into());
        assert!(gmail_for(bad).is_err());
    }
}
