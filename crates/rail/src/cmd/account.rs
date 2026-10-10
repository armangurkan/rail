use std::path::{Path, PathBuf};

use color_eyre::eyre::{WrapErr, bail, eyre};
use rail_core::auth::{
    AuthError, SecretError, SecretStore, TokenSet, endpoints_for, http_client, open_secret_store,
    run_auth_code_flow, unix_now,
};
use rail_core::config::{self, AccountConfig, AccountKind, Config, ConfigError, SecretsBackend};
use rail_core::paths::{Paths, write_private_atomic};
use rail_core::store::Store;
use url::Url;

use crate::browser::open_in_browser;
use crate::cli::{AddArgs, KindArg};
use crate::test_hooks;

impl From<KindArg> for AccountKind {
    fn from(kind: KindArg) -> Self {
        match kind {
            KindArg::Gmail => AccountKind::Gmail,
            KindArg::M365 => AccountKind::M365,
        }
    }
}

/// An account to add, before its name is settled. The flag form and the wizard both build
/// one. No `Debug`: it holds the client secret.
pub(crate) struct NewAccount {
    pub kind: AccountKind,
    pub address: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub tenant: Option<String>,
    /// `None` derives the name from the address.
    pub name: Option<String>,
    pub color: Option<String>,
}

impl From<AddArgs> for NewAccount {
    fn from(args: AddArgs) -> Self {
        Self {
            kind: args.kind.into(),
            address: args.address,
            client_id: args.client_id,
            client_secret: args.client_secret,
            tenant: args.tenant,
            name: args.name,
            color: args.color,
        }
    }
}

impl From<AccountConfig> for NewAccount {
    fn from(account: AccountConfig) -> Self {
        Self {
            kind: account.kind,
            address: account.address,
            client_id: account.client_id,
            client_secret: account.client_secret,
            tenant: account.tenant,
            name: Some(account.name),
            color: account.color,
        }
    }
}

/// Current config text and parsed value; an absent file counts as empty.
pub(crate) fn load_or_empty(paths: &Paths) -> color_eyre::Result<(String, Config)> {
    let path = paths.config_file();
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let config = config::parse(&text, &path)?;
            Ok((text, config))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Ok((String::new(), Config::default()))
        }
        Err(e) => Err(ConfigError::Io { path, source: e }.into()),
    }
}

/// Write `text` to `path` readable by the owner only. The config may hold a
/// `client_secret`, so the directory is 0700 and the file 0600. The write is
/// atomic: a crash or full disk leaves the previous config intact.
pub(crate) fn write_config(path: &Path, text: &str) -> std::io::Result<()> {
    write_private_atomic(path, text.as_bytes())
}

/// A validated account ready for login: the config text to write once it is stored.
pub(crate) struct PendingAdd {
    pub(crate) account: AccountConfig,
    pub(crate) new_text: String,
    pub(crate) config_path: PathBuf,
    pub(crate) backend: SecretsBackend,
}

/// Order matters: everything that can be checked offline is checked before the
/// browser opens, and config.toml is written last so a failure never leaves an
/// account in config that `add` would then refuse as a duplicate.
pub async fn add(paths: &Paths, args: AddArgs) -> color_eyre::Result<()> {
    let pending = prepare(paths, NewAccount::from(args))?;
    let secrets = open_secret_store(pending.backend, paths);
    probe_secrets(secrets.as_ref(), &pending.account.address)
        .wrap_err("secret storage is not usable")?;
    let tokens = login(&pending.account, open_in_browser)
        .await
        .wrap_err("login failed")?;
    persist(paths, secrets.as_ref(), &pending, &tokens)?;
    warn_if_file_backend(paths, pending.backend);
    let account = &pending.account;
    println!(
        "Added {} ({}) as \"{}\". Tokens stored in the {}.",
        account.address,
        account.kind.as_str(),
        account.name,
        secrets.backend_name()
    );
    Ok(())
}

/// Refuse a duplicate address and build the new config text, all before any login.
pub(crate) fn prepare(paths: &Paths, new: NewAccount) -> color_eyre::Result<PendingAdd> {
    let config_path = paths.config_file();
    let (text, config) = load_or_empty(paths)?;
    if config
        .accounts
        .iter()
        .any(|a| a.address.eq_ignore_ascii_case(&new.address))
    {
        bail!(
            "{} is already configured in {}; run `rail account login {}` to sign in again",
            new.address,
            config_path.display(),
            new.address
        );
    }
    let account = AccountConfig {
        name: new
            .name
            .unwrap_or_else(|| config::default_account_name(&new.address, &config)),
        kind: new.kind,
        address: new.address.to_ascii_lowercase(),
        color: new.color,
        client_id: new.client_id,
        client_secret: new.client_secret,
        tenant: new.tenant,
        signature: None,
    };
    // Pure: rejects a bad or duplicate-name account.
    let new_text = config::append_account(&text, &config_path, &account)?;
    Ok(PendingAdd {
        account,
        new_text,
        config_path,
        backend: config.general.secrets,
    })
}

/// Read-only reachability probe, so a broken secret store fails before the browser opens.
pub(crate) fn probe_secrets(secrets: &dyn SecretStore, address: &str) -> Result<(), SecretError> {
    secrets.get_tokens(address).map(|_| ())
}

/// The browser login for `account`. `open_url` receives the consent URL.
pub(crate) async fn login(
    account: &AccountConfig,
    open_url: impl FnOnce(&Url),
) -> Result<TokenSet, AuthError> {
    let mut endpoints = endpoints_for(account.kind, account.tenant.as_deref());
    if let Some(token_url) = test_hooks::token_url() {
        endpoints.token_url = token_url;
    }
    let http = http_client()?;
    run_auth_code_flow(account, &endpoints, &http, open_url, unix_now()).await
}

/// Tokens, then the store row, then config.toml last.
pub(crate) fn persist(
    paths: &Paths,
    secrets: &dyn SecretStore,
    pending: &PendingAdd,
    tokens: &TokenSet,
) -> color_eyre::Result<()> {
    secrets
        .set_tokens(&pending.account.address, tokens)
        .wrap_err("storing tokens")?;
    let store = Store::open(&paths.db_file())?;
    store.upsert_account(&pending.account)?;
    write_config(&pending.config_path, &pending.new_text)
        .wrap_err_with(|| format!("writing {}", pending.config_path.display()))
}

/// The login half of `persist` for an account already in config: tokens, then the store
/// row (so an account whose row was lost gets it back). Never writes config.toml.
pub(crate) fn persist_tokens_only(
    paths: &Paths,
    secrets: &dyn SecretStore,
    account: &AccountConfig,
    tokens: &TokenSet,
) -> color_eyre::Result<()> {
    secrets
        .set_tokens(&account.address, tokens)
        .wrap_err("storing tokens")?;
    Store::open(&paths.db_file())?.upsert_account(account)?;
    Ok(())
}

fn warn_if_file_backend(paths: &Paths, backend: SecretsBackend) {
    if backend == SecretsBackend::File {
        eprintln!(
            "warning: tokens are stored in {} (mode 0600) because secrets = \"file\"; prefer the OS keyring where available",
            paths.secrets_file().display()
        );
    }
}

/// The account whose name or address is `who`, ignoring ASCII case.
pub(crate) fn find_account<'a>(config: &'a Config, who: &str) -> Option<&'a AccountConfig> {
    config
        .accounts
        .iter()
        .find(|a| a.name.eq_ignore_ascii_case(who) || a.address.eq_ignore_ascii_case(who))
}

/// `rail account login <name-or-address>`: log in again and replace the stored tokens.
/// An unknown account fails before the browser opens.
pub async fn relogin(paths: &Paths, who: &str) -> color_eyre::Result<()> {
    let config_path = paths.config_file();
    let config = config::load(&config_path)?;
    let account = find_account(&config, who).cloned().ok_or_else(|| {
        eyre!(
            "no account named or addressed {who:?} in {}; run `rail account list` to see the configured accounts",
            config_path.display()
        )
    })?;
    let secrets = open_secret_store(config.general.secrets, paths);
    probe_secrets(secrets.as_ref(), &account.address).wrap_err("secret storage is not usable")?;
    let tokens = login(&account, open_in_browser)
        .await
        .wrap_err("login failed")?;
    persist_tokens_only(paths, secrets.as_ref(), &account, &tokens)?;
    warn_if_file_backend(paths, config.general.secrets);
    println!(
        "Logged in to {} ({}) as \"{}\". Tokens stored in the {}.",
        account.address,
        account.kind.as_str(),
        account.name,
        secrets.backend_name()
    );
    Ok(())
}

pub fn list(paths: &Paths) -> color_eyre::Result<()> {
    let config = config::load(&paths.config_file())?;
    let secrets = open_secret_store(config.general.secrets, paths);
    if config.accounts.is_empty() {
        println!("No accounts configured. Run `rail account add --help`.");
        return Ok(());
    }
    println!("{:<12} {:<6} {:<32} TOKENS", "NAME", "KIND", "ADDRESS");
    for account in &config.accounts {
        let tokens = match secrets.get_tokens(&account.address) {
            Ok(Some(_)) => "yes".to_string(),
            Ok(None) => "missing".to_string(),
            Err(e) => format!("error: {e}"),
        };
        println!(
            "{:<12} {:<6} {:<32} {tokens}",
            account.name,
            account.kind.as_str(),
            account.address
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rail_core::auth::MemorySecretStore;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    fn pending(paths: &Paths) -> PendingAdd {
        let account = AccountConfig {
            name: "me".into(),
            kind: AccountKind::Gmail,
            address: "me@example.com".into(),
            color: None,
            client_id: "cid".into(),
            client_secret: None,
            tenant: None,
            signature: None,
        };
        let new_text = config::append_account("", &paths.config_file(), &account).unwrap();
        PendingAdd {
            account,
            new_text,
            config_path: paths.config_file(),
            backend: SecretsBackend::Keyring,
        }
    }

    fn tokens() -> TokenSet {
        TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 0,
        }
    }

    fn file_backend_paths(tmp: &tempfile::TempDir, accounts: &str) -> Paths {
        let paths = Paths::all_in(tmp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        let text = format!("[general]\nsecrets = \"file\"\n{accounts}");
        std::fs::write(paths.config_file(), text).unwrap();
        paths
    }

    const WORK: &str = "\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";

    fn new_account(address: &str, name: Option<&str>) -> NewAccount {
        NewAccount {
            kind: AccountKind::Gmail,
            address: address.into(),
            client_id: "cid".into(),
            client_secret: Some("sec".into()),
            tenant: None,
            name: name.map(str::to_string),
            color: None,
        }
    }

    #[test]
    fn find_account_matches_name_or_address_ignoring_case() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = file_backend_paths(&tmp, WORK);
        let config = config::load(&paths.config_file()).unwrap();
        assert_eq!(find_account(&config, "WORK").unwrap().name, "work");
        assert_eq!(
            find_account(&config, "Me@Example.com").unwrap().name,
            "work"
        );
        assert!(find_account(&config, "nobody").is_none());
    }

    #[test]
    fn prepare_refuses_a_configured_address() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = file_backend_paths(&tmp, WORK);
        let err = prepare(&paths, new_account("ME@example.com", None))
            .err()
            .expect("duplicate refused");
        assert!(err.to_string().contains("already configured"), "{err}");
    }

    #[test]
    fn prepare_derives_a_name_and_appends_the_account() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = file_backend_paths(&tmp, WORK);
        let pending = prepare(&paths, new_account("Dana@Corp.com", None)).unwrap();
        assert_eq!(pending.account.name, "dana");
        assert_eq!(pending.account.address, "dana@corp.com");
        assert_eq!(pending.backend, SecretsBackend::File);
        let parsed = config::parse(&pending.new_text, &pending.config_path).unwrap();
        assert_eq!(parsed.accounts.len(), 2);
        let named = prepare(&paths, new_account("x@corp.com", Some("corp"))).unwrap();
        assert_eq!(named.account.name, "corp");
    }

    #[test]
    fn persist_tokens_only_replaces_tokens_and_never_touches_config() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = file_backend_paths(&tmp, WORK);
        let before = std::fs::read_to_string(paths.config_file()).unwrap();
        let config = config::load(&paths.config_file()).unwrap();
        let account = config.accounts[0].clone();
        let secrets = rail_core::auth::FileSecretStore::new(paths.secrets_file());
        secrets.set_tokens(&account.address, &tokens()).unwrap();
        let fresh = TokenSet {
            access_token: "a2".into(),
            refresh_token: "r2".into(),
            expires_at: 9,
        };
        persist_tokens_only(&paths, &secrets, &account, &fresh).unwrap();
        assert_eq!(secrets.get_tokens(&account.address).unwrap(), Some(fresh));
        let rows = Store::open(&paths.db_file())
            .unwrap()
            .list_accounts()
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            std::fs::read_to_string(paths.config_file()).unwrap(),
            before
        );
    }

    #[test]
    fn persist_writes_store_row_before_config() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(tmp.path());
        let secrets = MemorySecretStore::default();
        persist(&paths, &secrets, &pending(&paths), &tokens()).unwrap();
        assert!(secrets.get_tokens("me@example.com").unwrap().is_some());
        let rows = Store::open(&paths.db_file())
            .unwrap()
            .list_accounts()
            .unwrap();
        assert_eq!(rows.len(), 1);
        let loaded = config::load(&paths.config_file()).unwrap();
        assert_eq!(loaded.accounts[0].address, "me@example.com");
    }

    #[test]
    fn persist_leaves_config_untouched_when_the_store_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(tmp.path());
        // A directory where the database should be makes Store::open fail.
        std::fs::create_dir_all(paths.db_file()).unwrap();
        let secrets = MemorySecretStore::default();
        assert!(persist(&paths, &secrets, &pending(&paths), &tokens()).is_err());
        assert!(!paths.config_file().exists(), "config must be written last");
    }

    #[cfg(unix)]
    #[test]
    fn write_config_creates_private_dir_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/rail/config.toml");
        write_config(&path, "a = 1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\n");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    #[cfg(unix)]
    #[test]
    fn write_config_tightens_and_truncates_an_existing_file() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "a very long previous body\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_config(&path, "b = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b = 2\n");
        assert_eq!(mode(&path), 0o600);
    }
}
