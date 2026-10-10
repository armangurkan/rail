//! Switching the token backend (`secrets.configure`). Tokens are copied first and read back
//! from the new backend; only then is config written and the old copies deleted, so a failure
//! part way leaves the old copies and the old config in charge.

use std::path::PathBuf;

use super::onepassword::ItemIdCache;

use super::secrets::{SecretStore, open_secret_store};
use crate::config::{Config, OnePasswordConfig, SecretsBackend, load_text_or_empty, set_secrets};
use crate::error::ErrorClass;
use crate::ops::write_config_text;
use crate::ops::{ErrorCode, OpError, SecretsConfigureRequest, SecretsConfigureResponse};
use crate::paths::Paths;

const ONEPASSWORD: &str = "1password";

/// The message of the refusal for a change nobody confirmed.
pub const NEEDS_CONFIRMATION: &str =
    "changing where tokens are stored needs the user's confirmation";

/// Shown in a confirmation when tokens end up in the plain file.
pub const FILE_WARNING: &str =
    "Tokens will be stored in a plain 0600 file instead of the system keyring or 1Password.";

/// What a `secrets.configure` request would do, for the confirmation a front end shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchSummary {
    /// False when the request leaves every setting as it is.
    pub changes: bool,
    pub previous: SecretsBackend,
    pub backend: SecretsBackend,
    pub vault: Option<String>,
    pub account: Option<String>,
    /// Accounts whose tokens would be copied to the new store.
    pub accounts_moving: usize,
}

impl SwitchSummary {
    /// The text to put in front of the user before they confirm.
    pub fn text(&self) -> String {
        let mut text = format!(
            "Change token storage from {} to {}",
            self.previous.as_str(),
            self.backend.as_str()
        );
        if self.backend == SecretsBackend::OnePassword {
            let vault = self.vault.as_deref().unwrap_or("");
            text.push_str(&format!(" (vault \"{vault}\""));
            if let Some(account) = &self.account {
                text.push_str(&format!(", account \"{account}\""));
            }
            text.push(')');
        }
        text.push_str(&format!(
            ". Tokens for {} account(s) will be moved.",
            self.accounts_moving
        ));
        if self.backend == SecretsBackend::File {
            text.push(' ');
            text.push_str(FILE_WARNING);
        }
        text
    }
}

struct Plan {
    path: PathBuf,
    text: String,
    config: Config,
    target: Config,
}

impl Plan {
    fn load(paths: &Paths, request: &SecretsConfigureRequest) -> Result<Self, OpError> {
        validate_request(request)?;
        let path = paths.config_file();
        let (text, config) = load_text_or_empty(&path)?;
        let target = target_config(&config, request)?;
        Ok(Self {
            path,
            text,
            config,
            target,
        })
    }

    fn changes(&self) -> bool {
        self.target.general.secrets != self.config.general.secrets
            || self.target.onepassword != self.config.onepassword
    }

    /// Whether tokens would live somewhere else: another backend, or another vault or account.
    fn stores_differ(&self) -> bool {
        if self.target.general.secrets != self.config.general.secrets {
            return true;
        }
        let place = |c: &Config| {
            c.onepassword
                .as_ref()
                .map(|o| (o.vault.clone(), o.account.clone()))
        };
        self.target.general.secrets == SecretsBackend::OnePassword
            && place(&self.target) != place(&self.config)
    }
}

fn validate_request(request: &SecretsConfigureRequest) -> Result<(), OpError> {
    if request
        .op_account
        .as_deref()
        .is_some_and(|a| a.trim().is_empty())
    {
        return Err(OpError::invalid_input("op_account must not be empty"));
    }
    match request.vault.as_deref() {
        Some(v) if v.trim().is_empty() => Err(OpError::invalid_input("vault must not be empty")),
        Some(v) if v.contains('/') => Err(OpError::invalid_input("vault must not contain '/'")),
        _ => Ok(()),
    }
}

/// What `request` would change and how many accounts' tokens would move, without changing
/// anything. A front end shows `SwitchSummary::text` and sets `confirm` after the user agrees.
pub fn describe_switch(
    paths: &Paths,
    ids: &ItemIdCache,
    request: &SecretsConfigureRequest,
) -> Result<SwitchSummary, OpError> {
    let plan = Plan::load(paths, request)?;
    let onepassword = plan.target.onepassword.as_ref();
    let moving = if plan.changes() && plan.stores_differ() && request.migrate {
        let old = open_secret_store(&plan.config, paths, ids);
        present_accounts(&plan.config, old.as_ref()).len()
    } else {
        0
    };
    Ok(SwitchSummary {
        changes: plan.changes(),
        previous: plan.config.general.secrets,
        backend: request.backend,
        vault: onepassword.map(|o| o.vault.clone()),
        account: onepassword.and_then(|o| o.account.clone()),
        accounts_moving: moving,
    })
}

/// Point config at `request.backend` and, when asked, move every account's tokens there.
/// Refuses a change that is not `confirm`ed. The caller holds the config lock.
pub fn switch_backend(
    paths: &Paths,
    ids: &ItemIdCache,
    request: &SecretsConfigureRequest,
) -> Result<SecretsConfigureResponse, OpError> {
    let plan = Plan::load(paths, request)?;
    let mut response = SecretsConfigureResponse {
        backend: request.backend,
        previous: plan.config.general.secrets,
        moved: Vec::new(),
        old_copies_kept: Vec::new(),
        already_in_place: Vec::new(),
        config_path: plan.path.clone(),
    };
    if !plan.changes() {
        return Ok(response);
    }
    if !request.confirm {
        return Err(unconfirmed(paths, ids, request));
    }
    let new_text = set_secrets(
        &plan.text,
        &plan.path,
        request.backend,
        plan.target.onepassword.as_ref(),
    )?;
    let old = open_secret_store(&plan.config, paths, ids);
    let new = open_secret_store(&plan.target, paths, ids);
    new.probe()?;
    let moving = plan.stores_differ();
    if moving && request.migrate {
        response.moved = copy_tokens(&plan.config, old.as_ref(), new.as_ref())?;
    }
    write_config_text(&plan.path, &new_text)?;
    if moving && request.migrate {
        let retired = retire_old_copies(old.as_ref(), new.as_ref(), &response.moved);
        response.moved.retain(|a| !retired.in_place.contains(a));
        response.old_copies_kept = retired.kept;
        response.already_in_place = retired.in_place;
    } else if moving {
        response.old_copies_kept = present_accounts(&plan.config, old.as_ref());
    }
    Ok(response)
}

fn unconfirmed(paths: &Paths, ids: &ItemIdCache, request: &SecretsConfigureRequest) -> OpError {
    let error = OpError::invalid_input(NEEDS_CONFIRMATION);
    match describe_switch(paths, ids, request) {
        Ok(summary) => error.with_hint(summary.text()),
        Err(_) => error,
    }
}

/// The accounts that have tokens in `store`. One it cannot read counts as holding them: an
/// old copy that might exist is reported rather than assumed gone.
fn present_accounts(config: &Config, store: &dyn SecretStore) -> Vec<String> {
    config
        .accounts
        .iter()
        .filter(|a| {
            store
                .has_tokens(&a.address)
                .inspect_err(|e| tracing::warn!(error = %e, "could not check an account's tokens"))
                .unwrap_or(true)
        })
        .map(|a| a.address.clone())
        .collect()
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

/// What became of the old copies after a move.
struct Retired {
    /// Old copies that could not be deleted or confirmed gone.
    kept: Vec<String>,
    /// Accounts whose old and new settings name the same item: nothing was deleted.
    in_place: Vec<String>,
}

/// Delete what was moved, once it is sure that it is a different item from the new copy.
/// A copy that will not delete is reported, not fatal: the new backend already holds the
/// tokens and config points at it.
fn retire_old_copies(old: &dyn SecretStore, new: &dyn SecretStore, moved: &[String]) -> Retired {
    let mut retired = Retired {
        kept: Vec::new(),
        in_place: Vec::new(),
    };
    for address in moved {
        match retire_one(old, new, address) {
            Outcome::Deleted => {}
            Outcome::Kept => retired.kept.push(address.clone()),
            Outcome::InPlace => retired.in_place.push(address.clone()),
        }
    }
    retired
}

enum Outcome {
    Deleted,
    Kept,
    InPlace,
}

/// Item stores are checked with uncached lookups (nothing revealed): the old copy is the
/// new one when the ids match, and "no such item" on delete only counts when a fresh lookup
/// agrees the item is gone.
fn retire_one(old: &dyn SecretStore, new: &dyn SecretStore, address: &str) -> Outcome {
    let by_item = old.backend_name() == ONEPASSWORD;
    if by_item {
        // If the new copy cannot be identified, the old one cannot be told apart from it
        // (they may be the same item), so it stays.
        let new_id = if new.backend_name() == ONEPASSWORD {
            match new.item_id(address) {
                Ok(id) => id,
                Err(e) => return kept(&e, "could not look up the new token copy"),
            }
        } else {
            None
        };
        match old.item_id(address) {
            Err(e) => return kept(&e, "could not look up an old token copy"),
            Ok(Some(id)) if new_id.as_ref() == Some(&id) => return Outcome::InPlace,
            Ok(None) => return Outcome::Deleted,
            Ok(Some(_)) => {}
        }
    }
    if let Err(e) = old.delete_tokens(address) {
        return kept(&e, "could not delete an old token copy");
    }
    if by_item && !matches!(old.item_id(address), Ok(None)) {
        tracing::warn!("an old token copy is still there after its delete");
        return Outcome::Kept;
    }
    Outcome::Deleted
}

fn kept(error: &super::secrets::SecretError, what: &str) -> Outcome {
    tracing::warn!(error = %error, "{what}");
    Outcome::Kept
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
            confirm: true,
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
        assert_eq!(done.old_copies_kept, ["a@x.com", "b@x.com"]);
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
            confirm: false,
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
            confirm: true,
        };
        let err = switch_backend(&paths, &ItemIdCache::default(), &request).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert!(err.hint.unwrap().contains("vault"));
    }

    #[test]
    fn an_unconfirmed_change_is_refused_with_nothing_written() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, fake, file, text) = setup(&tmp);
        let request = SecretsConfigureRequest {
            confirm: false,
            ..to_onepassword()
        };
        let err = switch_backend(&paths, &ItemIdCache::default(), &request).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
        assert_eq!(err.class, ErrorClass::Permanent);
        assert_eq!(err.message, NEEDS_CONFIRMATION);
        assert!(err.hint.unwrap().contains("2 account(s)"));
        assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), text);
        assert_eq!(file.get_tokens("a@x.com").unwrap(), Some(tokens(1)));
        assert!(fake.items().is_empty());
    }

    #[test]
    fn the_summary_names_the_target_and_warns_about_the_plain_file() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, _fake, _file, _text) = setup(&tmp);
        let ids = ItemIdCache::default();
        let summary = describe_switch(&paths, &ids, &to_onepassword()).unwrap();
        assert!(summary.changes);
        assert_eq!(summary.accounts_moving, 2);
        let text = summary.text();
        assert!(text.contains("to 1password (vault \"Rail\")"), "{text}");
        assert!(text.contains("2 account(s)"), "{text}");
        assert!(!text.contains(FILE_WARNING));
        switch_backend(&paths, &ids, &to_onepassword()).unwrap();
        let back = SecretsConfigureRequest {
            backend: SecretsBackend::File,
            vault: None,
            op_account: None,
            migrate: true,
            confirm: true,
        };
        let text = describe_switch(&paths, &ids, &back).unwrap().text();
        assert!(text.ends_with(FILE_WARNING), "{text}");
    }

    #[test]
    fn changing_only_the_vault_moves_the_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, fake, _file, _text) = setup(&tmp);
        let ids = ItemIdCache::default();
        switch_backend(&paths, &ids, &to_onepassword()).unwrap();
        let other = SecretsConfigureRequest {
            vault: Some("Other".into()),
            ..to_onepassword()
        };
        let done = switch_backend(&paths, &ids, &other).unwrap();
        assert_eq!(done.moved, ["a@x.com", "b@x.com"]);
        assert!(done.old_copies_kept.is_empty());
        let config = load(&paths.config_file()).unwrap();
        assert_eq!(config.onepassword.as_ref().unwrap().vault, "Other");
        let store = OnePasswordSecretStore::new(fake.cli(), "Other".into(), ids.clone());
        assert_eq!(store.get_tokens("a@x.com").unwrap(), Some(tokens(1)));
        let old = OnePasswordSecretStore::new(fake.cli(), "Rail".into(), ids);
        assert_eq!(old.get_tokens("a@x.com").unwrap(), None);
    }

    #[test]
    fn bad_vault_and_account_values_are_invalid_input() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, _fake, _file, text) = setup(&tmp);
        for request in [
            SecretsConfigureRequest {
                vault: Some("a/b".into()),
                ..to_onepassword()
            },
            SecretsConfigureRequest {
                op_account: Some("  ".into()),
                ..to_onepassword()
            },
        ] {
            let err = switch_backend(&paths, &ItemIdCache::default(), &request).unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidInput, "{err:?}");
        }
        assert_eq!(std::fs::read_to_string(paths.config_file()).unwrap(), text);
    }

    fn account_cli(fake: &FakeOp, account: &str) -> crate::auth::OpCli {
        let config = OnePasswordConfig {
            account: Some(account.into()),
            ..fake.config()
        };
        crate::auth::OpCli::from_config(Some(&config), &fake.runtime_dir)
    }

    /// Both accounts hold tokens in vault "Rail" of 1Password account `account`.
    fn on_1password(tmp: &tempfile::TempDir, account: Option<&str>) -> (Paths, FakeOp) {
        let paths = Paths::all_in(tmp.path());
        let fake = FakeOp::install(tmp.path());
        let account_line = account.map_or(String::new(), |a| format!("account = \"{a}\"\n"));
        let table = fake.config_toml().replace(
            "vault = \"Rail\"\n",
            &format!("vault = \"Rail\"\n{account_line}"),
        );
        let text = format!(
            "{}{table}",
            TWO_ACCOUNTS.replace("secrets = \"file\"", "secrets = \"1password\"")
        );
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), text).unwrap();
        let cli = match account {
            Some(a) => account_cli(&fake, a),
            None => fake.cli(),
        };
        let store = OnePasswordSecretStore::new(cli, "Rail".into(), ItemIdCache::default());
        store.set_tokens("a@x.com", &tokens(1)).unwrap();
        store.set_tokens("b@x.com", &tokens(2)).unwrap();
        (paths, fake)
    }

    fn same_backend_other_account(account: &str, vault: Option<&str>) -> SecretsConfigureRequest {
        SecretsConfigureRequest {
            vault: vault.map(Into::into),
            op_account: Some(account.into()),
            ..to_onepassword()
        }
    }

    #[test]
    fn another_account_with_the_same_vault_name_gets_the_tokens_and_loses_the_old_copies() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, fake) = on_1password(&tmp, Some("A"));
        let ids = ItemIdCache::default();
        let request = same_backend_other_account("B", None);
        let done = switch_backend(&paths, &ids, &request).unwrap();
        assert_eq!(done.moved, ["a@x.com", "b@x.com"]);
        assert!(done.old_copies_kept.is_empty(), "{done:?}");
        assert!(done.already_in_place.is_empty());
        assert_eq!(fake.item_count("A", "rail"), 0, "old copies deleted");
        assert_eq!(fake.item_count("B", "rail"), 2, "new copies written");
        let new = OnePasswordSecretStore::new(account_cli(&fake, "B"), "Rail".into(), ids);
        assert_eq!(new.get_tokens("b@x.com").unwrap(), Some(tokens(2)));
    }

    #[test]
    fn one_store_named_two_ways_deletes_nothing() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, fake) = on_1password(&tmp, None);
        let ids = ItemIdCache::default();
        let request = same_backend_other_account("main", Some("rail"));
        let done = switch_backend(&paths, &ids, &request).unwrap();
        assert!(done.moved.is_empty(), "{done:?}");
        assert_eq!(done.already_in_place, ["a@x.com", "b@x.com"]);
        assert!(done.old_copies_kept.is_empty());
        assert_eq!(fake.items().len(), 2, "the only copies survive");
        let config = load(&paths.config_file()).unwrap();
        let store = OnePasswordSecretStore::new(fake.cli(), config.onepassword.unwrap().vault, ids);
        assert_eq!(store.get_tokens("a@x.com").unwrap(), Some(tokens(1)));
    }

    #[test]
    fn an_unverifiable_new_copy_keeps_the_old_one() {
        let tmp = tempfile::tempdir().unwrap();
        let (paths, fake) = on_1password(&tmp, Some("A"));
        // Account B's third `item list` on fails: after both copies, before they are checked.
        fake.fail_list_from("B", 3);
        let ids = ItemIdCache::default();
        let done = switch_backend(&paths, &ids, &same_backend_other_account("B", None)).unwrap();
        assert_eq!(done.old_copies_kept, ["a@x.com", "b@x.com"], "{done:?}");
        assert!(done.already_in_place.is_empty());
        assert_eq!(fake.item_count("A", "rail"), 2, "nothing was deleted");
        let old = OnePasswordSecretStore::new(account_cli(&fake, "A"), "Rail".into(), ids);
        assert_eq!(old.get_tokens("a@x.com").unwrap(), Some(tokens(1)));
    }
}
