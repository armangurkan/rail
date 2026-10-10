use color_eyre::eyre::{WrapErr, bail, eyre};
use rail_core::accounts;
use rail_core::auth::{ItemIdCache, http_client, open_secret_store};
use rail_core::config::{self, AccountKind, SecretsBackend};
use rail_core::ops::{AccountAddRequest, ClientSecretInput, OpError};
use rail_core::paths::Paths;

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

/// An operation error as the terminal shows it: the message, then its hint.
pub(crate) fn report(error: OpError) -> color_eyre::Report {
    eyre!(error.text())
}

/// The flag form of `rail account add`, or `None` when no flag asks for it and the wizard
/// should open (no arguments, or only a provider). A partial set of flags is an error that
/// names the missing one. `--client-secret op://...` is a 1Password reference.
pub(crate) fn new_account_from_args(
    args: AddArgs,
) -> color_eyre::Result<Option<AccountAddRequest>> {
    let Some(address) = args.address else {
        let others = [
            ("--client-id", args.client_id.is_some()),
            ("--client-secret", args.client_secret.is_some()),
            ("--tenant", args.tenant.is_some()),
            ("--name", args.name.is_some()),
            ("--color", args.color.is_some()),
        ];
        if let Some((flag, _)) = others.iter().find(|(_, given)| *given) {
            bail!("{flag} needs --address");
        }
        return Ok(None);
    };
    let Some(kind) = args.kind else {
        bail!("--address needs a provider: `rail account add <gmail|m365> --address ...`");
    };
    let Some(client_id) = args.client_id else {
        bail!("--address needs --client-id");
    };
    Ok(Some(AccountAddRequest {
        kind: kind.into(),
        address,
        client_id: Some(client_id),
        client_secret: args.client_secret.map(ClientSecretInput::from_flag),
        tenant: args.tenant,
        name: args.name,
        color: args.color,
        store_secret_in_1password: false,
        open_browser: true,
    }))
}

/// `rail account add` with flags: validate, log in, then tokens, store row and config.
pub async fn add(paths: &Paths, request: AccountAddRequest) -> color_eyre::Result<()> {
    let http = http_client().map_err(|e| report(e.into()))?;
    let prepared = accounts::prepare_and_probe(paths, &ItemIdCache::default(), &request, &http)
        .await
        .map_err(report)?;
    let token_url = test_hooks::token_url();
    let tokens = accounts::run_login(
        &prepared.login_account(),
        token_url.as_deref(),
        &http,
        open_in_browser,
    )
    .await
    .wrap_err("login failed")?;
    let saved = accounts::persist_new(paths, &ItemIdCache::default(), &prepared, &tokens)
        .map_err(report)?;
    warn_if_file_backend(paths, &saved.token_backend);
    println!(
        "Added {} ({}) as \"{}\". Tokens stored in the {}.",
        saved.address,
        saved.kind.as_str(),
        saved.name,
        saved.token_backend
    );
    Ok(())
}

fn warn_if_file_backend(paths: &Paths, backend: &str) {
    if backend == SecretsBackend::File.as_str() {
        eprintln!(
            "warning: tokens are stored in {} (mode 0600) because secrets = \"file\"; prefer the OS keyring where available",
            paths.secrets_file().display()
        );
    }
}

/// `rail account login <name-or-address>`: log in again and replace the stored tokens.
/// An unknown account fails before the browser opens.
pub async fn relogin(paths: &Paths, who: &str) -> color_eyre::Result<()> {
    let (config, account) = accounts::account_for_login(paths, who).map_err(report)?;
    let ids = ItemIdCache::default();
    let secrets = open_secret_store(&config, paths, &ids);
    accounts::probe_secrets(secrets.as_ref(), &account.address).map_err(report)?;
    let login = rail_core::config::AccountConfig {
        client_secret: accounts::login_secret(&account, &config, paths)
            .map_err(report)?
            .map(|s| s.expose().to_string()),
        ..account.clone()
    };
    let token_url = test_hooks::token_url();
    let http = http_client().map_err(|e| report(e.into()))?;
    let tokens = accounts::run_login(&login, token_url.as_deref(), &http, open_in_browser)
        .await
        .wrap_err("login failed")?;
    let saved = accounts::persist_login(paths, &ids, &config, &account, &tokens).map_err(report)?;
    warn_if_file_backend(paths, &saved.token_backend);
    println!(
        "Logged in to {} ({}) as \"{}\". Tokens stored in the {}.",
        saved.address,
        saved.kind.as_str(),
        saved.name,
        saved.token_backend
    );
    Ok(())
}

pub fn list(paths: &Paths) -> color_eyre::Result<()> {
    let config = config::load(&paths.config_file())?;
    let secrets = open_secret_store(&config, paths, &ItemIdCache::default());
    if config.accounts.is_empty() {
        println!("No accounts configured. Run `rail account add --help`.");
        return Ok(());
    }
    println!("{:<12} {:<6} {:<32} TOKENS", "NAME", "KIND", "ADDRESS");
    for account in &config.accounts {
        // Presence only: a 1Password store must not reveal the tokens to list accounts.
        let tokens = match secrets.has_tokens(&account.address) {
            Ok(true) => "yes".to_string(),
            Ok(false) => "missing".to_string(),
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
    use rail_core::ops::Secret;

    fn parse_add(args: &[&str]) -> AddArgs {
        use crate::cli::{AccountCommand, Cli, Command};
        use clap::Parser;
        match Cli::try_parse_from(args).unwrap().command {
            Some(Command::Account {
                command: AccountCommand::Add(add),
            }) => add,
            other => panic!("{other:?}"),
        }
    }

    fn from_args(args: &[&str]) -> color_eyre::Result<Option<AccountAddRequest>> {
        let mut all = vec!["rail", "account", "add"];
        all.extend_from_slice(args);
        new_account_from_args(parse_add(&all))
    }

    fn message(args: &[&str]) -> String {
        from_args(args)
            .expect_err("partial set refused")
            .to_string()
    }

    #[test]
    fn no_provider_and_no_flags_opens_the_wizard() {
        assert!(from_args(&[]).unwrap().is_none());
    }

    #[test]
    fn a_provider_alone_opens_the_wizard() {
        assert!(from_args(&["gmail"]).unwrap().is_none());
    }

    #[test]
    fn address_and_client_id_with_a_provider_build_the_account() {
        let flags = [
            "m365",
            "--address",
            "a@b.c",
            "--client-id",
            "x",
            "--tenant",
            "t",
        ];
        let new = from_args(&flags).unwrap().unwrap();
        assert_eq!(new.kind, AccountKind::M365);
        assert_eq!(new.address, "a@b.c");
        assert_eq!(new.client_id.as_deref(), Some("x"));
        assert_eq!(new.tenant.as_deref(), Some("t"));
        assert_eq!(new.name, None);
        assert!(new.open_browser);
    }

    #[test]
    fn a_client_secret_flag_may_be_a_1password_reference() {
        let base = [
            "gmail",
            "--address",
            "a@b.c",
            "--client-id",
            "x",
            "--client-secret",
        ];
        let mut reference = base.to_vec();
        reference.push("op://Rail/google/client_secret");
        assert_eq!(
            from_args(&reference).unwrap().unwrap().client_secret,
            Some(ClientSecretInput::Reference(
                "op://Rail/google/client_secret".into()
            ))
        );
        let mut value = base.to_vec();
        value.push("GOCSPX-x");
        assert_eq!(
            from_args(&value).unwrap().unwrap().client_secret,
            Some(ClientSecretInput::Value(Secret::new("GOCSPX-x")))
        );
    }

    #[test]
    fn address_without_a_provider_names_the_provider() {
        let msg = message(&["--address", "a@b.c", "--client-id", "x"]);
        assert!(msg.contains("provider"), "{msg}");
    }

    #[test]
    fn address_without_a_client_id_names_the_flag() {
        let msg = message(&["gmail", "--address", "a@b.c"]);
        assert!(msg.contains("--client-id"), "{msg}");
    }

    #[test]
    fn any_other_flag_without_an_address_names_the_address_flag() {
        for flag in [
            ["--client-id", "x"],
            ["--client-secret", "x"],
            ["--tenant", "x"],
            ["--name", "x"],
            ["--color", "x"],
        ] {
            let msg = message(&["gmail", flag[0], flag[1]]);
            assert!(msg.contains("--address"), "{flag:?}: {msg}");
        }
    }

    #[test]
    fn a_report_carries_the_hint() {
        let error = OpError::invalid_input("bad").with_hint("do this");
        assert_eq!(report(error).to_string(), "bad\nhint: do this");
    }
}
