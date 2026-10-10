use std::path::{Path, PathBuf};

use color_eyre::eyre::{WrapErr, bail};
use rail_core::auth::{
    SecretStore, TokenSet, endpoints_for, http_client, open_secret_store, run_auth_code_flow,
    unix_now,
};
use rail_core::config::{self, AccountConfig, AccountKind, Config, ConfigError, SecretsBackend};
use rail_core::paths::{Paths, write_private_atomic};
use rail_core::store::Store;

use crate::browser::open_in_browser;
use crate::cli::{AddArgs, KindArg};

impl From<KindArg> for AccountKind {
    fn from(kind: KindArg) -> Self {
        match kind {
            KindArg::Gmail => AccountKind::Gmail,
            KindArg::M365 => AccountKind::M365,
        }
    }
}

/// Current config text and parsed value; an absent file counts as empty.
fn load_or_empty(paths: &Paths) -> color_eyre::Result<(String, Config)> {
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
struct PendingAdd {
    account: AccountConfig,
    new_text: String,
    config_path: PathBuf,
    backend: SecretsBackend,
}

/// Order matters: everything that can be checked offline is checked before the
/// browser opens, and config.toml is written last so a failure never leaves an
/// account in config that `add` would then refuse as a duplicate.
pub async fn add(paths: &Paths, args: AddArgs) -> color_eyre::Result<()> {
    let pending = prepare(paths, args)?;
    let secrets = open_secret_store(pending.backend, paths);
    // Read-only reachability probe, so a broken secret store fails before the browser opens.
    secrets
        .get_tokens(&pending.account.address)
        .map_err(|e| color_eyre::eyre::eyre!("secret storage is not usable: {e}"))?;
    let tokens = login(&pending.account).await?;
    persist(paths, secrets.as_ref(), &pending, &tokens)?;
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
fn prepare(paths: &Paths, args: AddArgs) -> color_eyre::Result<PendingAdd> {
    let config_path = paths.config_file();
    let (text, config) = load_or_empty(paths)?;
    if config
        .accounts
        .iter()
        .any(|a| a.address.eq_ignore_ascii_case(&args.address))
    {
        bail!(
            "{} is already configured in {}; remove its [[accounts]] entry to add it again",
            args.address,
            config_path.display()
        );
    }
    let account = AccountConfig {
        name: args
            .name
            .unwrap_or_else(|| config::default_account_name(&args.address, &config)),
        kind: args.kind.into(),
        address: args.address.to_ascii_lowercase(),
        color: args.color,
        client_id: args.client_id,
        client_secret: args.client_secret,
        tenant: args.tenant,
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

async fn login(account: &AccountConfig) -> color_eyre::Result<TokenSet> {
    let endpoints = endpoints_for(account.kind, account.tenant.as_deref());
    let http = http_client()?;
    run_auth_code_flow(account, &endpoints, &http, open_in_browser, unix_now())
        .await
        .wrap_err("login failed")
}

/// Tokens, then the store row, then config.toml last.
fn persist(
    paths: &Paths,
    secrets: &dyn SecretStore,
    pending: &PendingAdd,
    tokens: &TokenSet,
) -> color_eyre::Result<()> {
    let address = &pending.account.address;
    secrets
        .set_tokens(address, tokens)
        .wrap_err("storing tokens")?;
    if pending.backend == SecretsBackend::File {
        eprintln!(
            "warning: tokens are stored in {} (mode 0600) because secrets = \"file\"; prefer the OS keyring where available",
            paths.secrets_file().display()
        );
    }
    let store = Store::open(&paths.db_file())?;
    store.upsert_account(&pending.account)?;
    write_config(&pending.config_path, &pending.new_text)
        .wrap_err_with(|| format!("writing {}", pending.config_path.display()))
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
