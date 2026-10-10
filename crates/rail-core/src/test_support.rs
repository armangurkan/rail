//! Shared fixtures for unit tests.

use crate::auth::TokenSet;
use crate::config::{AccountConfig, AccountKind, Config};
use crate::onboarding::{Action, DoneReport, Field, Input, Preset, Wizard};

pub(crate) fn sample_account() -> AccountConfig {
    AccountConfig {
        name: "work".to_string(),
        kind: AccountKind::Gmail,
        address: "me@example.com".to_string(),
        color: Some("blue".to_string()),
        client_id: "cid".to_string(),
        client_secret: None,
        tenant: None,
        signature: None,
        ai_send: None,
    }
}

/// A config with one account, "work" <work@example.com>.
pub(crate) fn existing_config() -> Config {
    Config {
        accounts: vec![AccountConfig {
            address: "work@example.com".to_string(),
            ..sample_account()
        }],
        ..Config::default()
    }
}

pub(crate) fn contoso_preset() -> Preset {
    Preset {
        name: Some("Contoso Microsoft 365".to_string()),
        kind: AccountKind::M365,
        client_id: "contoso-app".to_string(),
        tenant: Some("contoso.onmicrosoft.com".to_string()),
        client_secret: None,
        help_url: Some("https://intranet.contoso.example/rail".to_string()),
    }
}

pub(crate) fn wizard() -> Wizard {
    Wizard::new(Some(existing_config()), None)
}

/// Feed `inputs` in order and collect every action they produce.
pub(crate) fn walk(wizard: &mut Wizard, inputs: impl IntoIterator<Item = Input>) -> Vec<Action> {
    inputs.into_iter().flat_map(|i| wizard.handle(i)).collect()
}

pub(crate) fn edit(field: Field, value: &str) -> Input {
    Input::Edit(field, value.to_string())
}

/// From Start to the Details step of the bring-your-own path: 0 is Gmail, 1 Microsoft 365.
pub(crate) fn own_app(provider: usize) -> Vec<Input> {
    vec![Input::Choose(1), Input::Choose(provider), Input::Next]
}

pub(crate) fn gmail_details(address: &str) -> Vec<Input> {
    vec![
        edit(Field::Address, address),
        edit(Field::ClientId, "cid"),
        edit(Field::ClientSecret, "sec"),
    ]
}

pub(crate) fn m365_details(address: &str, tenant: &str) -> Vec<Input> {
    vec![
        edit(Field::Address, address),
        edit(Field::ClientId, "cid"),
        edit(Field::Tenant, tenant),
    ]
}

pub(crate) fn done_report() -> DoneReport {
    DoneReport {
        config_path: "/home/me/.config/rail/config.toml".into(),
        token_backend: "file".to_string(),
        token_location: "/home/me/.local/share/rail/secrets.json".to_string(),
        store_path: "/home/me/.local/share/rail/rail.db".into(),
    }
}

pub(crate) fn tokens() -> TokenSet {
    TokenSet {
        access_token: "access".to_string(),
        refresh_token: "refresh".to_string(),
        expires_at: 1,
    }
}

/// A daemon context over `paths` whose opener records every URL instead of starting a browser.
pub(crate) fn daemon_context(
    paths: &crate::paths::Paths,
) -> (
    crate::daemon::DaemonContext,
    tokio::sync::mpsc::UnboundedReceiver<url::Url>,
) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let opener: crate::daemon::Opener = std::sync::Arc::new(move |url: &url::Url| {
        let _ = tx.send(url.clone());
        Ok(())
    });
    let ctx = crate::daemon::DaemonContext::new(paths.clone(), opener).expect("http client");
    (ctx, rx)
}

/// Bind a real daemon on `paths` and run it on a task.
pub(crate) async fn start_daemon(
    ctx: crate::daemon::DaemonContext,
    config: crate::daemon::ServeConfig,
) -> tokio::task::JoinHandle<Result<(), crate::ops::OpError>> {
    let server = crate::daemon::Server::bind(ctx, config)
        .await
        .expect("bind the test daemon");
    tokio::spawn(server.run())
}

/// A connector whose spawn starts an in-process daemon (with the default config) and counts
/// the starts.
pub(crate) fn in_process_connector(
    paths: &crate::paths::Paths,
) -> (
    crate::daemon::Connector,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
) {
    use std::sync::atomic::Ordering;
    let spawned = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = spawned.clone();
    let for_spawn = paths.clone();
    let spawn: crate::daemon::SpawnFn = std::sync::Arc::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
        let (ctx, _urls) = daemon_context(&for_spawn);
        tokio::spawn(async move {
            match crate::daemon::Server::bind(ctx, crate::daemon::ServeConfig::default()).await {
                Ok(server) => {
                    let _ = server.run().await;
                }
                Err(e) => panic!("test daemon failed to bind: {e}"),
            }
        });
        Ok(())
    });
    (crate::daemon::Connector::new(paths.clone(), spawn), spawned)
}
