//! `presets.list`, `preset.fetch` and `account.remove` through a real daemon.

use std::sync::Arc;

use super::client::DaemonClient;
use super::{Connector, ServeConfig};
use crate::auth::{FileSecretStore, SecretStore, TokenSet};
use crate::config::load;
use crate::ops::op::{AccountRemove, PresetFetch, PresetsList};
use crate::ops::{AccountRemoveRequest, ErrorCode, PresetFetchRequest, PresetsListRequest};
use crate::paths::Paths;
use crate::store::Store;
use crate::test_support::{daemon_context, start_daemon};

const ACME: &str = "name = \"Acme Gmail\"\nkind = \"gmail\"\nclient_id = \"acme.apps.googleusercontent.com\"\nclient_secret = \"GOCSPX-acme\"\nhelp_url = \"https://intranet.example/rail\"\n";
const TWO: &str = "# mine\n[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n\n[[accounts]]\nname = \"corp\"\nkind = \"m365\"\naddress = \"me@corp.com\"\nclient_id = \"cid2\"\n";

async fn daemon(tmp: &tempfile::TempDir, config: &str) -> (Paths, DaemonClient) {
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), config).unwrap();
    let (ctx, _) = daemon_context(&paths);
    let _daemon = start_daemon(ctx, ServeConfig::default()).await;
    let client = Connector::new(paths.clone(), Arc::new(|| Ok(())))
        .connect()
        .await
        .unwrap();
    (paths, client)
}

fn tokens() -> TokenSet {
    TokenSet {
        access_token: "access".into(),
        refresh_token: "refresh".into(),
        expires_at: 1,
    }
}

#[tokio::test]
async fn presets_list_reports_presets_without_their_secrets() {
    let tmp = tempfile::tempdir().unwrap();
    let (paths, client) = daemon(&tmp, "").await;
    let dir = paths.config_dir.join("presets");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("acme.toml"), ACME).unwrap();
    std::fs::write(
        dir.join("bad.toml"),
        "kind = \"gmail\"\nnope = \"TOP-SECRET\"\n",
    )
    .unwrap();
    let listed = client
        .call::<PresetsList>(&PresetsListRequest {})
        .await
        .unwrap();
    let text = serde_json::to_string(&listed).unwrap();
    assert!(!text.contains("GOCSPX-acme"), "{text}");
    assert!(!text.contains("TOP-SECRET"), "{text}");
    let acme = listed
        .presets
        .iter()
        .find(|p| p.source.get().ends_with("acme.toml"))
        .unwrap();
    let summary = acme.preset.as_ref().unwrap();
    assert!(summary.has_client_secret);
    assert_eq!(summary.label.get(), "Acme Gmail");
    let bad = listed
        .presets
        .iter()
        .find(|p| p.source.get().ends_with("bad.toml"))
        .unwrap();
    assert!(
        bad.error.as_ref().unwrap().message.contains("bad.toml:2:"),
        "{bad:?}"
    );
}

#[tokio::test]
async fn preset_fetch_of_a_file_returns_its_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, client) = daemon(&tmp, "").await;
    let file = tmp.path().join("acme.toml");
    std::fs::write(&file, ACME).unwrap();
    let request = PresetFetchRequest {
        source: file.display().to_string(),
    };
    let fetched = client.call::<PresetFetch>(&request).await.unwrap();
    assert_eq!(
        fetched.preset.client_id.get(),
        "acme.apps.googleusercontent.com"
    );
    assert_eq!(
        fetched.preset.help_url.unwrap().get(),
        "https://intranet.example/rail"
    );
}

#[tokio::test]
async fn preset_fetch_refuses_plain_http() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, client) = daemon(&tmp, "").await;
    let request = PresetFetchRequest {
        source: "http://127.0.0.1:9/preset.toml".into(),
    };
    let err = client.call::<PresetFetch>(&request).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Preset);
    assert!(err.message.contains("https://"), "{err}");
}

/// Review focus 6: a preset source may be any file the user can read.
#[tokio::test]
async fn preset_fetch_of_a_file_that_is_not_a_preset_never_echoes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, client) = daemon(&tmp, "").await;
    let file = tmp.path().join("id_ed25519");
    std::fs::write(
        &file,
        "-----BEGIN OPENSSH PRIVATE KEY-----\nTOP-SECRET-KEY-MATERIAL\n",
    )
    .unwrap();
    let request = PresetFetchRequest {
        source: file.display().to_string(),
    };
    let err = client.call::<PresetFetch>(&request).await.unwrap_err();
    let text = serde_json::to_string(&err).unwrap();
    assert!(!text.contains("TOP-SECRET-KEY-MATERIAL"), "{text}");
    assert!(err.message.contains("not a valid Rail preset"), "{err}");
}

/// Review focus 4, at the operation.
#[tokio::test]
async fn remove_without_confirm_changes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (paths, client) = daemon(&tmp, TWO).await;
    let secrets = FileSecretStore::new(paths.secrets_file());
    secrets.set_tokens("me@example.com", &tokens()).unwrap();
    let config = load(&paths.config_file()).unwrap();
    Store::open(&paths.db_file())
        .unwrap()
        .sync_accounts_from_config(&config)
        .unwrap();
    let request = AccountRemoveRequest {
        account: "work".into(),
        confirm: false,
    };
    let err = client.call::<AccountRemove>(&request).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotConfirmed);
    assert!(
        err.message
            .contains("removing an account needs the user's confirmation"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), TWO);
    assert!(secrets.get_tokens("me@example.com").unwrap().is_some());
    let rows = Store::open(&paths.db_file()).unwrap().list_accounts();
    assert_eq!(rows.unwrap().len(), 2);
}

#[tokio::test]
async fn remove_deletes_the_entry_and_tokens_but_keeps_the_store_row() {
    let tmp = tempfile::tempdir().unwrap();
    let (paths, client) = daemon(&tmp, TWO).await;
    let secrets = FileSecretStore::new(paths.secrets_file());
    secrets.set_tokens("me@example.com", &tokens()).unwrap();
    let config = load(&paths.config_file()).unwrap();
    Store::open(&paths.db_file())
        .unwrap()
        .sync_accounts_from_config(&config)
        .unwrap();
    let request = AccountRemoveRequest {
        account: "ME@example.com".into(),
        confirm: true,
    };
    let removed = client.call::<AccountRemove>(&request).await.unwrap();
    assert_eq!(removed.name, "work");
    assert!(removed.tokens_removed && removed.mail_kept);
    let left = load(&paths.config_file()).unwrap();
    assert_eq!(left.accounts.len(), 1);
    assert_eq!(left.accounts[0].name, "corp");
    assert!(
        std::fs::read_to_string(paths.config_file())
            .unwrap()
            .contains("# mine")
    );
    assert_eq!(secrets.get_tokens("me@example.com").unwrap(), None);
    let rows = Store::open(&paths.db_file())
        .unwrap()
        .list_accounts()
        .unwrap();
    assert_eq!(rows.len(), 2, "Rail never deletes mail");
}

#[tokio::test]
async fn remove_of_an_unknown_account_is_not_found() {
    let tmp = tempfile::tempdir().unwrap();
    let (_, client) = daemon(&tmp, TWO).await;
    let request = AccountRemoveRequest {
        account: "nobody".into(),
        confirm: true,
    };
    let err = client.call::<AccountRemove>(&request).await.unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}
