use std::io::Write;
use std::path::Path;

use color_eyre::eyre::{WrapErr, bail};
use rail_core::auth::{
    endpoints_for, http_client, open_secret_store, run_auth_code_flow, unix_now,
};
use rail_core::config::{self, AccountConfig, AccountKind, Config, ConfigError, SecretsBackend};
use rail_core::paths::Paths;
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
/// `client_secret`, so the directory is created 0700 and the file 0600 (an
/// existing, looser file is tightened).
pub(crate) fn write_config(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(text.as_bytes())
}

pub async fn add(paths: &Paths, args: AddArgs) -> color_eyre::Result<()> {
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

    let endpoints = endpoints_for(account.kind, account.tenant.as_deref());
    let http = http_client()?;
    let tokens = run_auth_code_flow(&account, &endpoints, &http, open_in_browser, unix_now())
        .await
        .wrap_err("login failed")?;

    let secrets = open_secret_store(config.general.secrets, paths);
    secrets
        .set_tokens(&account.address, &tokens)
        .wrap_err("storing tokens")?;
    if config.general.secrets == SecretsBackend::File {
        eprintln!(
            "warning: tokens are stored in {} (mode 0600) because secrets = \"file\"; prefer the OS keyring where available",
            paths.secrets_file().display()
        );
    }

    let new_text = config::append_account(&text, &config_path, &account)?;
    write_config(&config_path, &new_text)
        .wrap_err_with(|| format!("writing {}", config_path.display()))?;

    let store = Store::open(&paths.db_file())?;
    store.upsert_account(&account)?;

    println!(
        "Added {} ({}) as \"{}\". Tokens stored in the {}.",
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn mode(path: &Path) -> u32 {
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn write_config_creates_private_dir_and_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested/rail/config.toml");
        write_config(&path, "a = 1\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "a = 1\n");
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
    }

    #[test]
    fn write_config_tightens_and_truncates_an_existing_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "a very long previous body\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_config(&path, "b = 2\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "b = 2\n");
        assert_eq!(mode(&path), 0o600);
    }
}
