//! Logging in to an account, and saving what a re-login produced.

use super::context;
use crate::auth::{
    ItemIdCache, OAuthEndpoints, OpCli, TokenSet, endpoints_for, open_secret_store, token_location,
};
use crate::config::{self, AccountConfig, Config};
use crate::ops::{OP_REFERENCE_PREFIX, OpError, SavedAccount, Secret};
use crate::paths::Paths;
use crate::store::Store;

/// The account whose name or address is `who`. An exact name or address match wins; otherwise
/// a match ignoring ASCII case must be unique, or the answer is an error, never the first hit.
pub fn find_account<'a>(config: &'a Config, who: &str) -> Result<&'a AccountConfig, OpError> {
    let exact = |a: &&AccountConfig| a.name == who || a.address == who;
    if let Some(account) = config.accounts.iter().find(exact) {
        return Ok(account);
    }
    let mut matches = config
        .accounts
        .iter()
        .filter(|a| a.name.eq_ignore_ascii_case(who) || a.address.eq_ignore_ascii_case(who));
    match (matches.next(), matches.next()) {
        (Some(account), None) => Ok(account),
        (Some(_), Some(_)) => Err(OpError::invalid_input(format!(
            "{who:?} matches more than one account ignoring case; use the exact name or address"
        ))),
        _ => Err(OpError::not_found(format!(
            "no account named or addressed {who:?}; run `rail account list` to see the configured accounts"
        ))),
    }
}

/// The config and the account `who` names. An unknown account fails before any browser opens.
pub fn account_for_login(paths: &Paths, who: &str) -> Result<(Config, AccountConfig), OpError> {
    let path = paths.config_file();
    let config = config::load(&path)?;
    let account = find_account(&config, who)?.clone();
    Ok((config, account))
}

/// The client secret a login uses: the configured value, or the value behind an `op://`
/// reference, read now and never cached.
pub fn login_secret(
    account: &AccountConfig,
    config: &Config,
    paths: &Paths,
) -> Result<Option<Secret>, OpError> {
    match &account.client_secret {
        Some(value) if value.starts_with(OP_REFERENCE_PREFIX) => {
            let op = OpCli::from_config(config.onepassword.as_ref(), &paths.runtime_dir);
            op.read_reference(value)
                .map(Some)
                .map_err(|e| context("reading the client secret from 1Password", e))
        }
        Some(value) => Ok(Some(Secret::new(value.clone()))),
        None => Ok(None),
    }
}

/// The provider's endpoints for `account`, with the token URL replaced when a test asks.
pub fn endpoints_for_login(account: &AccountConfig, token_url: Option<&str>) -> OAuthEndpoints {
    let mut endpoints = endpoints_for(account.kind, account.tenant.as_deref());
    if let Some(token_url) = token_url {
        endpoints.token_url = token_url.to_string();
    }
    endpoints
}

/// After a re-login: tokens, then the store row (so an account whose row was lost gets it
/// back). Never writes config.toml.
pub fn persist_login(
    paths: &Paths,
    ids: &ItemIdCache,
    config: &Config,
    account: &AccountConfig,
    tokens: &TokenSet,
) -> Result<SavedAccount, OpError> {
    open_secret_store(config, paths, ids)
        .set_tokens(&account.address, tokens)
        .map_err(|e| context("storing tokens", e))?;
    Store::open(&paths.db_file())?.upsert_account(account)?;
    Ok(saved_account(config, paths, account))
}

/// Where `account` now lives. Locations only, never token values.
pub fn saved_account(config: &Config, paths: &Paths, account: &AccountConfig) -> SavedAccount {
    SavedAccount {
        name: account.name.clone(),
        kind: account.kind,
        address: account.address.clone(),
        config_path: paths.config_file(),
        token_backend: config.general.secrets.as_str().to_string(),
        token_location: token_location(config, paths, &account.address),
        store_path: paths.db_file(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{FileSecretStore, SecretStore};
    use crate::test_support::FakeOp;

    const WORK: &str = "[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";

    fn paths_with(tmp: &tempfile::TempDir, text: &str) -> Paths {
        let paths = Paths::all_in(tmp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), text).unwrap();
        paths
    }

    #[test]
    fn find_account_matches_name_or_address_ignoring_case() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_with(&tmp, WORK);
        let (config, account) = account_for_login(&paths, "WORK").unwrap();
        assert_eq!(account.name, "work");
        assert_eq!(
            find_account(&config, "Me@Example.com").unwrap().name,
            "work"
        );
        let mut twin = config.accounts[0].clone();
        twin.name = "WORK".into();
        twin.address = "other@example.com".into();
        let mut both = config.clone();
        both.accounts.push(twin);
        assert_eq!(
            find_account(&both, "WORK").unwrap().address,
            "other@example.com"
        );
        assert_eq!(
            find_account(&both, "work").unwrap().address,
            "me@example.com"
        );
        let err = find_account(&both, "Work").unwrap_err();
        assert!(err.message.contains("more than one account"), "{err}");
        let err = account_for_login(&paths, "nobody").unwrap_err();
        assert!(
            err.message.contains("no account named or addressed"),
            "{err}"
        );
    }

    #[test]
    fn persist_login_replaces_tokens_and_never_touches_config() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_with(&tmp, WORK);
        let (config, account) = account_for_login(&paths, "work").unwrap();
        let fresh = TokenSet {
            access_token: "a2".into(),
            refresh_token: "r2".into(),
            expires_at: 9,
        };
        let saved =
            persist_login(&paths, &ItemIdCache::default(), &config, &account, &fresh).unwrap();
        assert_eq!(saved.token_backend, "file");
        let secrets = FileSecretStore::new(paths.secrets_file());
        assert_eq!(secrets.get_tokens("me@example.com").unwrap(), Some(fresh));
        assert_eq!(
            Store::open(&paths.db_file())
                .unwrap()
                .list_accounts()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), WORK);
    }

    #[test]
    fn an_op_reference_in_config_is_read_for_the_login() {
        let tmp = tempfile::tempdir().unwrap();
        let fake = FakeOp::install(tmp.path());
        fake.set_read("GOCSPX-live");
        let text = format!(
            "[[accounts]]\nname = \"w\"\nkind = \"gmail\"\naddress = \"w@example.com\"\nclient_id = \"c\"\nclient_secret = \"op://Rail/google/client_secret\"\n{}",
            fake.config_toml()
        );
        let paths = paths_with(&tmp, &text);
        let (config, account) = account_for_login(&paths, "w").unwrap();
        let secret = login_secret(&account, &config, &paths).unwrap().unwrap();
        assert_eq!(secret.expose(), "GOCSPX-live");
        assert!(
            fake.argv()
                .contains("read --no-newline op://Rail/google/client_secret")
        );
        let plain = AccountConfig {
            client_secret: Some("GOCSPX-plain".into()),
            ..account
        };
        assert_eq!(
            login_secret(&plain, &config, &paths)
                .unwrap()
                .unwrap()
                .expose(),
            "GOCSPX-plain"
        );
    }

    #[test]
    fn a_test_token_url_replaces_the_providers() {
        let (_, account) = {
            let tmp = tempfile::tempdir().unwrap();
            let paths = paths_with(&tmp, WORK);
            account_for_login(&paths, "work").unwrap()
        };
        assert_eq!(
            endpoints_for_login(&account, Some("http://127.0.0.1:9/token")).token_url,
            "http://127.0.0.1:9/token"
        );
        assert_eq!(
            endpoints_for_login(&account, None).token_url,
            "https://oauth2.googleapis.com/token"
        );
    }
}
