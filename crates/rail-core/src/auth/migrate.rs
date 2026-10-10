//! Switching the token backend (`secrets.configure`). Tokens are copied first and read back
//! from the new backend; only then is config written and the old copies deleted, so a failure
//! part way leaves the old copies and the old config in charge.

use super::onepassword::ItemIdCache;
use super::secrets::{SecretStore, open_secret_store};
use crate::config::{Config, OnePasswordConfig, SecretsBackend, load_text_or_empty, set_secrets};
use crate::error::ErrorClass;
use crate::ops::write_config_text;
use crate::ops::{ErrorCode, OpError, SecretsConfigureRequest, SecretsConfigureResponse};
use crate::paths::Paths;

/// Point config at `request.backend` and, when asked, move every account's tokens there.
/// The caller holds the config lock.
pub fn switch_backend(
    paths: &Paths,
    ids: &ItemIdCache,
    request: &SecretsConfigureRequest,
) -> Result<SecretsConfigureResponse, OpError> {
    let path = paths.config_file();
    let (text, config) = load_text_or_empty(&path)?;
    let target = target_config(&config, request)?;
    let mut response = SecretsConfigureResponse {
        backend: request.backend,
        previous: config.general.secrets,
        moved: Vec::new(),
        old_copies_kept: Vec::new(),
        config_path: path.clone(),
    };
    if target.general.secrets == config.general.secrets && target.onepassword == config.onepassword
    {
        return Ok(response);
    }
    let new_text = set_secrets(&text, &path, request.backend, target.onepassword.as_ref())?;
    let old = open_secret_store(&config, paths, ids);
    let new = open_secret_store(&target, paths, ids);
    new.probe()?;
    if request.migrate && target.general.secrets != config.general.secrets {
        response.moved = copy_tokens(&config, old.as_ref(), new.as_ref())?;
    }
    write_config_text(&path, &new_text)?;
    response.old_copies_kept = delete_old_copies(old.as_ref(), &response.moved);
    Ok(response)
}

fn target_config(config: &Config, request: &SecretsConfigureRequest) -> Result<Config, OpError> {
    let mut target = config.clone();
    target.general.secrets = request.backend;
    let wants_onepassword = request.backend == SecretsBackend::OnePassword
        || request.vault.is_some()
        || request.op_account.is_some();
    if wants_onepassword {
        target.onepassword = Some(onepassword_config(config.onepassword.as_ref(), request)?);
    }
    Ok(target)
}

fn onepassword_config(
    existing: Option<&OnePasswordConfig>,
    request: &SecretsConfigureRequest,
) -> Result<OnePasswordConfig, OpError> {
    let vault = request
        .vault
        .clone()
        .or_else(|| existing.map(|o| o.vault.clone()))
        .filter(|v| !v.trim().is_empty())
        .ok_or_else(|| {
            OpError::invalid_input("the 1password backend needs a vault").with_hint(
                "pass vault, for example \"Rail\"; Rail keeps one item per account there",
            )
        })?;
    Ok(OnePasswordConfig {
        vault,
        account: request
            .op_account
            .clone()
            .or_else(|| existing.and_then(|o| o.account.clone())),
        // Never from the request: only the user, editing config.toml, picks the program.
        op_path: existing.and_then(|o| o.op_path.clone()),
    })
}

/// Copy each account's tokens and check they read back. The first failure stops the move and
/// says which accounts had been copied: their old copies are untouched and config still points
/// at the old backend.
fn copy_tokens(
    config: &Config,
    old: &dyn SecretStore,
    new: &dyn SecretStore,
) -> Result<Vec<String>, OpError> {
    let mut moved = Vec::new();
    for account in &config.accounts {
        let address = account.address.as_str();
        copy_one(address, old, new).map_err(|e| partial_failure(e, &moved, config))?;
        moved.push(address.to_string());
    }
    Ok(moved)
}

/// Copy one account's tokens, if it has any, and read them back from `new`.
fn copy_one(address: &str, old: &dyn SecretStore, new: &dyn SecretStore) -> Result<(), OpError> {
    let Some(tokens) = old.get_tokens(address)? else {
        return Ok(());
    };
    new.set_tokens(address, &tokens)?;
    if new.get_tokens(address)?.as_ref() == Some(&tokens) {
        return Ok(());
    }
    Err(OpError::new(
        ErrorCode::Secrets,
        ErrorClass::Permanent,
        format!(
            "the tokens for {address} did not read back from the {} backend",
            new.backend_name()
        ),
    ))
}

/// `error`, extended with which accounts were copied before it and which were not reached.
/// Addresses come from the config; no token is involved.
fn partial_failure(mut error: OpError, moved: &[String], config: &Config) -> OpError {
    let rest: Vec<&str> = config
        .accounts
        .iter()
        .map(|a| a.address.as_str())
        .filter(|a| !moved.iter().any(|m| m == a))
        .collect();
    error.message = format!(
        "{}; nothing was switched. Copied before the failure (old copies kept): [{}]; not copied: [{}]",
        error.message,
        moved.join(", "),
        rest.join(", ")
    );
    error
}

/// Delete what was moved. A copy that will not delete is reported, not fatal: the new backend
/// already holds the tokens and config points at it.
fn delete_old_copies(old: &dyn SecretStore, moved: &[String]) -> Vec<String> {
    moved
        .iter()
        .filter(|address| {
            old.delete_tokens(address)
                .inspect_err(|e| tracing::warn!(error = %e, "could not delete an old token copy"))
                .is_err()
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{FileSecretStore, OnePasswordSecretStore, TokenSet};
    use crate::config::load;
    use crate::test_support::FakeOp;

    fn tokens(n: i64) -> TokenSet {
        TokenSet {
            access_token: format!("access-{n}"),
            refresh_token: format!("refresh-{n}"),
            expires_at: n,
        }
    }

    const TWO_ACCOUNTS: &str = "# mine\n[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"a\"\nkind = \"gmail\"\naddress = \"a@x.com\"\nclient_id = \"c\"\n\n[[accounts]]\nname = \"b\"\nkind = \"gmail\"\naddress = \"b@x.com\"\nclient_id = \"c\"\n";

    /// Two accounts on the file backend, with `op_path` set by hand to the fake, as a user
    /// would. Returns the config text as written.
    fn setup(tmp: &tempfile::TempDir) -> (Paths, FakeOp, FileSecretStore, String) {
        let paths = Paths::all_in(tmp.path());
        let fake = FakeOp::install(tmp.path());
        let text = format!("{TWO_ACCOUNTS}{}", fake.config_toml());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), &text).unwrap();
        let file = FileSecretStore::new(paths.secrets_file());
        file.set_tokens("a@x.com", &tokens(1)).unwrap();
        file.set_tokens("b@x.com", &tokens(2)).unwrap();
        (paths, fake, file, text)
    }

    fn to_onepassword() -> SecretsConfigureRequest {
        SecretsConfigureRequest {
            backend: SecretsBackend::OnePassword,
            vault: Some("Rail".into()),
            op_account: None,
            migrate: true,
        }
    }

    #[test]
    fn switching_to_1password_moves_tokens_and_deletes_the_old_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, fake, file, _text) = setup(&tmp);
        let ids = ItemIdCache::default();
        let done = switch_backend(&paths, &ids, &to_onepassword()).unwrap();
        assert_eq!(done.previous, SecretsBackend::File);
        assert_eq!(done.moved, ["a@x.com", "b@x.com"]);
        assert!(done.old_copies_kept.is_empty());
        assert_eq!(file.get_tokens("a@x.com").unwrap(), None);
        let config = load(&paths.config_file()).unwrap();
        assert_eq!(config.general.secrets, SecretsBackend::OnePassword);
        assert_eq!(config.onepassword.as_ref().unwrap().vault, "Rail");
        assert!(
            std::fs::read_to_string(paths.config_file())
                .unwrap()
                .contains("# mine")
        );
        let onepassword = OnePasswordSecretStore::new(fake.cli(), "Rail".into(), ids);
        assert_eq!(
            config.onepassword.as_ref().unwrap().op_path.as_deref(),
            Some(fake.path.as_path()),
            "the hand-set op_path is kept"
        );
        assert_eq!(onepassword.get_tokens("b@x.com").unwrap(), Some(tokens(2)));
    }

    #[test]
    fn a_failure_part_way_leaves_the_old_copies_and_the_config() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, fake, file, text) = setup(&tmp);
        fake.fail_on("b@x.com");
        let err = switch_backend(&paths, &ItemIdCache::default(), &to_onepassword()).unwrap_err();
        assert_eq!(err.code, ErrorCode::Secrets);
        assert!(!err.message.contains("access-2"), "{}", err.message);
        assert!(
            err.message
                .contains("Copied before the failure (old copies kept): [a@x.com]"),
            "{}",
            err.message
        );
        assert!(
            err.message.contains("not copied: [b@x.com]"),
            "{}",
            err.message
        );
        assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), text);
        assert_eq!(file.get_tokens("a@x.com").unwrap(), Some(tokens(1)));
        assert_eq!(file.get_tokens("b@x.com").unwrap(), Some(tokens(2)));
    }

    #[test]
    fn a_locked_1password_stops_the_switch_before_anything_moves() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, fake, file, text) = setup(&tmp);
        fake.lock();
        let err = switch_backend(&paths, &ItemIdCache::default(), &to_onepassword()).unwrap_err();
        assert!(err.hint.unwrap().contains("op signin"));
        assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), text);
        assert_eq!(file.get_tokens("a@x.com").unwrap(), Some(tokens(1)));
    }

    /// Review focus 6: no request field reaches `op_path`.
    #[test]
    fn a_request_cannot_choose_the_program_rail_runs() {
        let schema =
            serde_json::to_string(&schemars::schema_for!(SecretsConfigureRequest)).unwrap();
        assert!(!schema.contains("op_path"), "{schema}");
        let with_path =
            serde_json::json!({"backend": "1password", "vault": "Rail", "op_path": "/tmp/evil"});
        let request: SecretsConfigureRequest = serde_json::from_value(with_path).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(tmp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), TWO_ACCOUNTS).unwrap();
        let _ = switch_backend(&paths, &ItemIdCache::default(), &request);
        let written = std::fs::read_to_string(paths.config_file()).unwrap();
        assert!(!written.contains("/tmp/evil"), "{written}");
    }

    #[test]
    fn switching_without_migrate_only_changes_the_config() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, fake, file, _text) = setup(&tmp);
        let request = SecretsConfigureRequest {
            migrate: false,
            ..to_onepassword()
        };
        let done = switch_backend(&paths, &ItemIdCache::default(), &request).unwrap();
        assert!(done.moved.is_empty());
        assert_eq!(file.get_tokens("a@x.com").unwrap(), Some(tokens(1)));
        assert!(fake.items().is_empty());
    }

    #[test]
    fn the_same_backend_changes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, _fake, _file, text) = setup(&tmp);
        let request = SecretsConfigureRequest {
            backend: SecretsBackend::File,
            vault: None,
            op_account: None,
            migrate: true,
        };
        let done = switch_backend(&paths, &ItemIdCache::default(), &request).unwrap();
        assert!(done.moved.is_empty());
        assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), text);
    }

    #[test]
    fn onepassword_without_a_vault_is_invalid_input() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = Paths::all_in(tmp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), TWO_ACCOUNTS).unwrap();
        let request = SecretsConfigureRequest {
            backend: SecretsBackend::OnePassword,
            vault: None,
            op_account: None,
            migrate: true,
        };
        let err = switch_backend(&paths, &ItemIdCache::default(), &request).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.hint.unwrap().contains("vault"));
    }
}
