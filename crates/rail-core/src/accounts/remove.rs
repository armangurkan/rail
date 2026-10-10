//! Removing an account: its config entry, then its tokens. Its mail rows stay in the store;
//! Rail never deletes mail.

use super::login::account_for_login;
use crate::auth::{ItemIdCache, open_secret_store, token_location};
use crate::config::{self, load_text_or_empty};
use crate::ops::{AccountRemoveResponse, OpError, write_config_text};
use crate::paths::Paths;

/// What a trusted front end shows the person before it sets `confirm`: which account goes,
/// where its tokens are, and that its mail stays. The account name and address are the
/// user's own configuration, so this text is Rail's to print.
pub fn removal_summary(paths: &Paths, who: &str) -> Result<String, OpError> {
    let (config, account) = account_for_login(paths, who)?;
    Ok(format!(
        "Remove account {} <{}> ({})?\n\
         Its config entry is removed and its tokens are deleted from {}.\n\
         Mail already synced stays in the local store ({}); Rail never deletes mail.",
        account.name,
        account.address,
        account.kind.as_str(),
        token_location(&config, paths, &account.address),
        paths.db_file().display()
    ))
}

/// Remove the account `who` names. The config entry goes first, so a failure to delete the
/// tokens leaves no entry whose tokens are gone: the response says the tokens remain and the
/// config change stands. The caller holds the config lock and has the person's confirmation.
pub fn remove_account(
    paths: &Paths,
    ids: &ItemIdCache,
    who: &str,
) -> Result<AccountRemoveResponse, OpError> {
    let (config, account) = account_for_login(paths, who)?;
    let path = paths.config_file();
    let (text, _) = load_text_or_empty(&path)?;
    let new_text = config::remove_account(&text, &path, &account.name)?;
    write_config_text(&path, &new_text)?;
    let deleted = open_secret_store(&config, paths, ids).delete_tokens(&account.address);
    Ok(AccountRemoveResponse {
        name: account.name,
        kind: account.kind,
        address: account.address,
        tokens_removed: deleted.is_ok(),
        token_error: deleted.err().map(|e| e.to_string().into()),
        mail_kept: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{FileSecretStore, SecretStore, TokenSet};

    const WORK: &str = "[general]\nsecrets = \"file\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"cid\"\n";

    fn paths_with(tmp: &tempfile::TempDir) -> Paths {
        let paths = Paths::all_in(tmp.path());
        std::fs::create_dir_all(&paths.config_dir).unwrap();
        std::fs::write(paths.config_file(), WORK).unwrap();
        paths
    }

    #[test]
    fn the_summary_names_the_account_the_token_place_and_the_kept_mail() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_with(&tmp);
        let text = removal_summary(&paths, "WORK").unwrap();
        assert!(text.contains("work <me@example.com> (gmail)"), "{text}");
        assert!(text.contains(&paths.secrets_file().display().to_string()));
        assert!(text.contains("Mail already synced stays"), "{text}");
        assert!(removal_summary(&paths, "nobody").is_err());
    }

    #[test]
    fn a_token_delete_failure_is_reported_and_the_config_change_stands() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_with(&tmp);
        std::fs::create_dir_all(paths.secrets_file().parent().unwrap()).unwrap();
        std::fs::write(paths.secrets_file(), "TOP-SECRET not json").unwrap();
        let removed = remove_account(&paths, &ItemIdCache::default(), "work").unwrap();
        assert!(!removed.tokens_removed && removed.mail_kept);
        let reason = removed.token_error.unwrap();
        assert!(!reason.get().contains("TOP-SECRET"), "{reason:?}");
        let left = config::load(&paths.config_file()).unwrap();
        assert!(left.accounts.is_empty());
    }

    #[test]
    fn a_clean_removal_deletes_the_tokens() {
        let tmp = tempfile::tempdir().unwrap();
        let paths = paths_with(&tmp);
        let secrets = FileSecretStore::new(paths.secrets_file());
        let tokens = TokenSet {
            access_token: "a".into(),
            refresh_token: "r".into(),
            expires_at: 1,
        };
        secrets.set_tokens("me@example.com", &tokens).unwrap();
        let removed = remove_account(&paths, &ItemIdCache::default(), "work").unwrap();
        assert!(removed.tokens_removed && removed.token_error.is_none());
        assert_eq!(secrets.get_tokens("me@example.com").unwrap(), None);
    }
}
