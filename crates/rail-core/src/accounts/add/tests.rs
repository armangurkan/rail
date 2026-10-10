use super::*;
use crate::auth::{FileSecretStore, MemorySecretStore};
use crate::config::SecretsBackend;
use crate::ops::ClientSecretInput;
use crate::test_support::FakeOp;

const WORK: &str = "[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";

fn paths_with(tmp: &tempfile::TempDir, text: &str) -> Paths {
    let paths = Paths::all_in(tmp.path());
    std::fs::create_dir_all(&paths.config_dir).unwrap();
    std::fs::write(paths.config_file(), text).unwrap();
    paths
}

fn request(address: &str) -> AccountAddRequest {
    AccountAddRequest {
        kind: AccountKind::Gmail,
        address: address.to_string(),
        client_id: Some("cid".into()),
        client_secret: Some(ClientSecretInput::Value(Secret::new("sec"))),
        tenant: None,
        name: None,
        color: None,
        store_secret_in_1password: false,
        open_browser: false,
    }
}

fn value(secret: &str) -> ResolvedSecret {
    ResolvedSecret {
        login: Some(Secret::new(secret)),
        stored: Some(Secret::new(secret)),
        ..ResolvedSecret::default()
    }
}

fn tokens() -> TokenSet {
    TokenSet {
        access_token: "a".into(),
        refresh_token: "r".into(),
        expires_at: 1,
    }
}

#[test]
fn prepare_refuses_a_configured_address() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let err = prepare_add(&paths, &request("ME@example.com"), value("s"))
        .err()
        .unwrap();
    assert_eq!(err.code, ErrorCode::Duplicate);
    assert!(err.message.contains("already configured"), "{err}");
    assert!(err.message.contains("config.toml"), "{err}");
}

#[test]
fn prepare_refuses_a_duplicate_name_with_the_config_error() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let named = AccountAddRequest {
        name: Some("work".into()),
        ..request("other@example.com")
    };
    let err = prepare_add(&paths, &named, value("s")).err().unwrap();
    // The config parser's wording is outside text: it is in `detail`, not `message`.
    assert!(err.text().contains("duplicate account name"), "{err:?}");
}

#[test]
fn prepare_derives_a_name_and_takes_the_client_id_from_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let from_file = ResolvedSecret {
        client_id: Some("file-cid".into()),
        ..value("GOCSPX-f")
    };
    let no_id = AccountAddRequest {
        client_id: None,
        ..request("Dana@Corp.com")
    };
    let prepared = prepare_add(&paths, &no_id, from_file).unwrap();
    assert_eq!(prepared.account.name, "dana");
    assert_eq!(prepared.account.address, "dana@corp.com");
    assert_eq!(prepared.account.client_id, "file-cid");
    let mismatch = ResolvedSecret {
        client_id: Some("other".into()),
        ..value("s")
    };
    let err = prepare_add(&paths, &request("x@corp.com"), mismatch)
        .err()
        .unwrap();
    assert!(err.message.contains("does not match"), "{err}");
    let none = AccountAddRequest {
        client_id: None,
        ..request("y@corp.com")
    };
    assert!(prepare_add(&paths, &none, value("s")).is_err());
}

#[test]
fn a_reference_is_what_config_keeps_and_the_value_is_what_the_login_uses() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let resolved = ResolvedSecret {
        login: Some(Secret::new("GOCSPX-real")),
        stored: Some(Secret::new("op://Rail/google/client_secret")),
        ..ResolvedSecret::default()
    };
    let prepared = prepare_add(&paths, &request("n@x.com"), resolved).unwrap();
    assert_eq!(
        prepared.account.client_secret.as_deref(),
        Some("op://Rail/google/client_secret")
    );
    assert_eq!(
        prepared.login_account().client_secret.as_deref(),
        Some("GOCSPX-real")
    );
}

#[test]
fn persist_writes_tokens_then_the_store_row_then_config() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, "[general]\nsecrets = \"file\"\n");
    let prepared = prepare_add(&paths, &request("me@example.com"), value("sec")).unwrap();
    let saved = persist_new(&paths, &ItemIdCache::default(), &prepared, &tokens()).unwrap();
    assert_eq!(saved.name, "me");
    assert_eq!(
        saved.token_location,
        paths.secrets_file().display().to_string()
    );
    let secrets = FileSecretStore::new(paths.secrets_file());
    assert_eq!(
        secrets.get_tokens("me@example.com").unwrap(),
        Some(tokens())
    );
    assert_eq!(
        Store::open(&paths.db_file())
            .unwrap()
            .list_accounts()
            .unwrap()
            .len(),
        1
    );
    let loaded = config::load(&paths.config_file()).unwrap();
    assert_eq!(loaded.accounts[0].client_secret.as_deref(), Some("sec"));
}

#[test]
fn persist_leaves_config_untouched_when_the_store_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, "[general]\nsecrets = \"file\"\n");
    let prepared = prepare_add(&paths, &request("me@example.com"), value("sec")).unwrap();
    std::fs::create_dir_all(paths.db_file()).unwrap();
    assert!(persist_new(&paths, &ItemIdCache::default(), &prepared, &tokens()).is_err());
    assert_eq!(
        std::fs::read_to_string(paths.config_file()).unwrap(),
        "[general]\nsecrets = \"file\"\n",
        "config is written last"
    );
}

#[test]
fn persist_refuses_an_address_added_since_prepare() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, "[general]\nsecrets = \"file\"\n");
    let prepared = prepare_add(&paths, &request("me@example.com"), value("sec")).unwrap();
    std::fs::write(paths.config_file(), WORK).unwrap();
    let err = persist_new(&paths, &ItemIdCache::default(), &prepared, &tokens()).unwrap_err();
    assert_eq!(err.code, ErrorCode::Duplicate);
    assert!(
        !paths.secrets_file().exists(),
        "nothing stored for a refused save"
    );
}

#[test]
fn a_secret_store_failure_names_storing_tokens_and_writes_no_config() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, "[general]\nsecrets = \"file\"\n");
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::write(paths.secrets_file(), "{not json").unwrap();
    let prepared = prepare_add(&paths, &request("me@example.com"), value("sec")).unwrap();
    let err = persist_new(&paths, &ItemIdCache::default(), &prepared, &tokens()).unwrap_err();
    assert!(err.message.starts_with("storing tokens: "), "{err}");
    assert!(err.message.contains("corrupt"), "{err}");
    assert_eq!(err.class, ErrorClass::Permanent);
}

#[test]
fn probe_secrets_wraps_the_cause() {
    let ok = MemorySecretStore::default();
    assert!(probe_secrets(&ok, "a@x.com").is_ok());
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("secrets.json");
    std::fs::write(&path, "{not json").unwrap();
    let err = probe_secrets(&FileSecretStore::new(path), "a@x.com").unwrap_err();
    assert!(
        err.message.starts_with("secret storage is not usable: "),
        "{err}"
    );
}

#[test]
fn the_client_secret_can_go_to_1password_with_a_reference_in_config() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeOp::install(tmp.path());
    let paths = paths_with(
        &tmp,
        &format!("[general]\nsecrets = \"file\"\n{}", fake.config_toml()),
    );
    let wants = AccountAddRequest {
        store_secret_in_1password: true,
        ..request("me@example.com")
    };
    let prepared = prepare_add(&paths, &wants, value("GOCSPX-to-vault")).unwrap();
    persist_new(&paths, &ItemIdCache::default(), &prepared, &tokens()).unwrap();
    let config_text = std::fs::read_to_string(paths.config_file()).unwrap();
    assert!(
        config_text.contains("client_secret = \"op://Rail/item1/client_secret\""),
        "{config_text}"
    );
    assert!(!config_text.contains("GOCSPX-to-vault"), "{config_text}");
    assert!(fake.items()[0].contains("GOCSPX-to-vault"));
    assert!(!fake.argv().contains("GOCSPX-to-vault"));
    assert_eq!(
        config::load(&paths.config_file()).unwrap().general.secrets,
        SecretsBackend::File
    );
}

#[test]
fn storing_in_1password_needs_a_vault() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, "[general]\nsecrets = \"file\"\n");
    let wants = AccountAddRequest {
        store_secret_in_1password: true,
        ..request("me@example.com")
    };
    let err = prepare_add(&paths, &wants, value("s")).err().unwrap();
    assert!(err.message.contains("1Password vault"), "{err}");
}

#[tokio::test]
async fn prepare_and_probe_resolves_checks_and_probes_in_one_call() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let http = crate::auth::http_client().unwrap();
    let ids = ItemIdCache::default();
    let ok = prepare_and_probe(&paths, &ids, &request("new@example.com"), &http)
        .await
        .unwrap();
    assert_eq!(ok.account.client_secret.as_deref(), Some("sec"));
    let dup = prepare_and_probe(&paths, &ids, &request("me@example.com"), &http).await;
    assert_eq!(dup.err().unwrap().code, ErrorCode::Duplicate);
    std::fs::create_dir_all(&paths.data_dir).unwrap();
    std::fs::write(paths.secrets_file(), "{not json").unwrap();
    let broken = prepare_and_probe(&paths, &ids, &request("new@example.com"), &http).await;
    let err = broken.err().unwrap();
    assert!(
        err.message.starts_with("secret storage is not usable: "),
        "{err}"
    );
}

#[tokio::test]
async fn probing_never_reveals_1password_tokens() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeOp::install(tmp.path());
    let text = format!("[general]\nsecrets = \"1password\"\n{}", fake.config_toml());
    let paths = paths_with(&tmp, &text);
    let http = crate::auth::http_client().unwrap();
    let ids = ItemIdCache::default();
    let config = config::load(&paths.config_file()).unwrap();
    open_secret_store(&config, &paths, &ids)
        .set_tokens("new@example.com", &tokens())
        .unwrap();
    std::fs::remove_file(fake.state.join("argv.log")).unwrap();
    prepare_and_probe(&paths, &ids, &request("new@example.com"), &http)
        .await
        .unwrap();
    assert!(fake.argv().contains("item list"), "{}", fake.argv());
    assert!(!fake.argv().contains("--reveal"), "{}", fake.argv());
}

#[tokio::test]
async fn a_duplicate_address_is_refused_before_op_is_run() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeOp::install(tmp.path());
    let paths = paths_with(&tmp, &format!("{WORK}{}", fake.config_toml()));
    let wants = AccountAddRequest {
        client_secret: Some(ClientSecretInput::Reference(
            "op://Rail/g/client_secret".into(),
        )),
        ..request("ME@example.com")
    };
    let http = crate::auth::http_client().unwrap();
    let err = resolve_and_prepare(&paths, &wants, &http)
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, ErrorCode::Duplicate);
    assert_eq!(fake.argv(), "", "no op call for a duplicate address");
}

/// Final review M7: a client secret that is already an `op://` reference is not copied into
/// the Rail vault: the agent could name any item it likes. Refused before `op` runs.
#[tokio::test]
async fn storing_a_reference_in_1password_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = FakeOp::install(tmp.path());
    let paths = paths_with(&tmp, &format!("{WORK}{}", fake.config_toml()));
    let wants = AccountAddRequest {
        client_secret: Some(ClientSecretInput::Reference(
            "op://Private/someone-else/password".into(),
        )),
        store_secret_in_1password: true,
        ..request("new@example.com")
    };
    let http = crate::auth::http_client().unwrap();
    let err = resolve_and_prepare(&paths, &wants, &http)
        .await
        .err()
        .unwrap();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    assert_eq!(
        err.message,
        "the client secret is already in 1Password; drop store_secret_in_1password"
    );
    assert_eq!(fake.argv(), "", "op never ran");
    let resolved = ResolvedSecret {
        login: Some(Secret::new("GOCSPX-real")),
        stored: Some(Secret::new("op://Private/someone-else/password")),
        ..ResolvedSecret::default()
    };
    let err = prepare_add(&paths, &wants, resolved).err().unwrap();
    assert_eq!(err.code, ErrorCode::InvalidInput, "{err}");
}

/// Final review M2: the address, colour and tenant are shown in confirmations and on the
/// terminal, so spaces and control characters (an OSC 52 clipboard escape, say) are refused
/// at add, without echoing the value.
#[test]
fn whitespace_and_control_characters_are_refused_at_add() {
    let tmp = tempfile::tempdir().unwrap();
    let paths = paths_with(&tmp, WORK);
    let osc52 = "\u{1b}]52;c;ZXZpbA==\u{7}me@example.com";
    for address in [
        osc52,
        "me @example.com",
        "me@exam\tple.com",
        "me@example.com\nx",
    ] {
        let err = prepare_add(&paths, &request(address), value("s"))
            .err()
            .unwrap();
        assert_eq!(err.code, ErrorCode::InvalidInput, "{address:?}");
        assert!(err.message.contains("address"), "{err}");
        assert!(
            !err.message.contains('\u{1b}') && !err.message.contains("ZXZp"),
            "{err}"
        );
    }
    let colored = AccountAddRequest {
        color: Some("blue\u{1b}[31m".into()),
        ..request("c@example.com")
    };
    let err = prepare_add(&paths, &colored, value("s")).err().unwrap();
    assert!(err.message.contains("color"), "{err}");
    let tenant = AccountAddRequest {
        kind: AccountKind::M365,
        tenant: Some("contoso .onmicrosoft.com".into()),
        ..request("t@example.com")
    };
    let err = prepare_add(&paths, &tenant, value("s")).err().unwrap();
    assert!(err.message.contains("tenant"), "{err}");
    assert!(std::fs::read_to_string(paths.config_file()).unwrap() == WORK);
    let fine = AccountAddRequest {
        color: Some("blue".into()),
        ..request("  Fine@Example.com ")
    };
    assert!(prepare_add(&paths, &fine, value("s")).is_ok());
}
