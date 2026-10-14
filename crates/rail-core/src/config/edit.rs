//! Edit `config.toml` without destroying the user's comments and formatting.

use std::path::Path;

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use super::load::{ConfigError, line_col, parse};
use super::model::{AccountConfig, Config, OnePasswordConfig, SecretsBackend};

fn document(text: &str, path: &Path) -> Result<DocumentMut, ConfigError> {
    text.parse().map_err(|e: toml_edit::TomlError| {
        let (line, col) = e.span().map_or((0, 0), |s| line_col(text, s.start));
        ConfigError::Parse {
            path: path.to_path_buf(),
            line,
            col,
            message: e.message().to_string(),
        }
    })
}

fn not_a_table(path: &Path, key: &str) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_path_buf(),
        message: format!("`{key}` must be a table ([{key}])"),
    }
}

/// Return `text` with `[general] secrets` set to `backend` and, when given, the
/// `[onepassword]` table replaced. Comments elsewhere are kept. Validates the result.
pub fn set_secrets(
    text: &str,
    path: &Path,
    backend: SecretsBackend,
    onepassword: Option<&OnePasswordConfig>,
) -> Result<String, ConfigError> {
    let mut doc = document(text, path)?;
    let general = doc
        .entry("general")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| not_a_table(path, "general"))?;
    general["secrets"] = value(backend.as_str());
    if let Some(op) = onepassword {
        write_onepassword(&mut doc, op);
    }
    let out = doc.to_string();
    parse(&out, path)?;
    Ok(out)
}

/// Update `[onepassword]` in place: an existing table keeps its comments and any key not named
/// here; a missing one is created. `op_path` is written only when present, unchanged otherwise.
fn write_onepassword(doc: &mut DocumentMut, op: &OnePasswordConfig) {
    if !doc.get("onepassword").is_some_and(Item::is_table) {
        doc["onepassword"] = Item::Table(Table::new());
    }
    let Some(table) = doc["onepassword"].as_table_mut() else {
        return;
    };
    set_string(table, "vault", &op.vault);
    match &op.account {
        Some(account) => set_string(table, "account", account),
        None => {
            table.remove("account");
        }
    }
    if let Some(op_path) = &op.op_path {
        set_string(table, "op_path", &op_path.to_string_lossy());
    }
}

/// Set `key` to a string, keeping the formatting and trailing comment of an existing value.
fn set_string(table: &mut Table, key: &str, text: &str) {
    set_value(table, key, toml_edit::Value::from(text));
}

/// Set `key` to `new`, keeping the formatting and trailing comment of an existing value.
fn set_value(table: &mut Table, key: &str, mut new: toml_edit::Value) {
    let decor = table
        .get(key)
        .and_then(Item::as_value)
        .map(|v| v.decor().clone());
    if let Some(decor) = decor {
        *new.decor_mut() = decor;
    }
    table[key] = Item::Value(new);
}

/// Return `text` with `[general] key` set to `value`, or removed when `value` is `None`. The
/// table is created when absent; an existing value keeps its formatting and trailing comment;
/// comments and other keys stay. Validates the result.
pub fn set_general(
    text: &str,
    path: &Path,
    key: &str,
    value: Option<toml_edit::Value>,
) -> Result<String, ConfigError> {
    let mut doc = document(text, path)?;
    let general = doc
        .entry("general")
        .or_insert(Item::Table(Table::new()))
        .as_table_mut()
        .ok_or_else(|| not_a_table(path, "general"))?;
    match value {
        Some(new) => set_value(general, key, new),
        None => {
            general.remove(key);
        }
    }
    let out = doc.to_string();
    parse(&out, path)?;
    Ok(out)
}

/// `set_general` for a text value.
pub fn set_general_text(
    text: &str,
    path: &Path,
    key: &str,
    value: Option<&str>,
) -> Result<String, ConfigError> {
    set_general(text, path, key, value.map(Into::into))
}

/// `set_general` for an integer value.
pub fn set_general_number(
    text: &str,
    path: &Path,
    key: &str,
    value: Option<i64>,
) -> Result<String, ConfigError> {
    set_general(text, path, key, value.map(Into::into))
}

/// `set_general` for a boolean value.
pub fn set_general_bool(
    text: &str,
    path: &Path,
    key: &str,
    value: Option<bool>,
) -> Result<String, ConfigError> {
    set_general(text, path, key, value.map(Into::into))
}

/// Return `text` with a new `[[accounts]]` table appended. Validates the result.
pub fn append_account(
    text: &str,
    path: &Path,
    account: &AccountConfig,
) -> Result<String, ConfigError> {
    let mut doc = document(text, path)?;
    let accounts = doc
        .entry("accounts")
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| ConfigError::Invalid {
            path: path.to_path_buf(),
            message: "`accounts` must be an array of tables ([[accounts]])".to_string(),
        })?;

    let mut table = Table::new();
    table["name"] = value(account.name.as_str());
    table["kind"] = value(account.kind.as_str());
    table["address"] = value(account.address.as_str());
    if let Some(color) = &account.color {
        table["color"] = value(color.as_str());
    }
    table["client_id"] = value(account.client_id.as_str());
    if let Some(secret) = &account.client_secret {
        table["client_secret"] = value(secret.as_str());
    }
    if let Some(tenant) = &account.tenant {
        table["tenant"] = value(tenant.as_str());
    }
    if let Some(signature) = &account.signature {
        table["signature"] = value(signature.to_string_lossy().into_owned());
    }
    if let Some(name) = &account.from_name {
        table["from_name"] = value(name.as_str());
    }
    if let Some(ai_send) = account.ai_send {
        table["ai_send"] = value(ai_send.as_str());
    }
    if let Some(days) = account.history_days {
        table["history_days"] = value(i64::from(days));
    }
    if let Some(days) = account.body_cache_days {
        table["body_cache_days"] = value(i64::from(days));
    }
    accounts.push(table);

    let out = doc.to_string();
    parse(&out, path)?;
    Ok(out)
}

/// Return `text` without the `[[accounts]]` table named `name`. Other accounts, other tables
/// and comments stay. Validates the result.
pub fn remove_account(text: &str, path: &Path, name: &str) -> Result<String, ConfigError> {
    let mut doc = document(text, path)?;
    let accounts = doc
        .get_mut("accounts")
        .and_then(Item::as_array_of_tables_mut)
        .ok_or_else(|| no_such_account(path, name))?;
    let index = accounts
        .iter()
        .position(|t| t.get("name").and_then(Item::as_str) == Some(name))
        .ok_or_else(|| no_such_account(path, name))?;
    accounts.remove(index);
    let out = doc.to_string();
    parse(&out, path)?;
    Ok(out)
}

fn no_such_account(path: &Path, name: &str) -> ConfigError {
    ConfigError::Invalid {
        path: path.to_path_buf(),
        message: format!("no [[accounts]] entry named {name:?}"),
    }
}

/// `dana.reyes+x@corp.com` -> `dana_reyes_x`, suffixed `-2`, `-3`... if taken.
pub fn default_account_name(address: &str, config: &Config) -> String {
    let local = address
        .split('@')
        .next()
        .unwrap_or(address)
        .to_ascii_lowercase();
    let base: String = local
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base = if base.is_empty() {
        "account".to_string()
    } else {
        base
    };
    let taken = |name: &str| config.accounts.iter().any(|a| a.name == name);
    if !taken(&base) {
        return base;
    }
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !taken(candidate))
        .expect("unbounded range always yields a free name")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountKind, parse};
    use std::path::Path;

    fn account(name: &str, address: &str) -> AccountConfig {
        AccountConfig {
            name: name.into(),
            kind: AccountKind::M365,
            address: address.into(),
            color: Some("magenta".into()),
            label: None,
            tenant: Some("common".into()),
            ..crate::test_support::sample_account()
        }
    }

    const EXISTING: &str = "# my rail config\n[general]\neditor = \"nvim\" # the good one\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"g\"\n";

    #[test]
    fn append_preserves_comments_and_adds_a_table() {
        let out = append_account(
            EXISTING,
            Path::new("c.toml"),
            &account("corp", "me@corp.com"),
        )
        .unwrap();
        assert!(out.contains("# my rail config"));
        assert!(out.contains("# the good one"));
        let config = parse(&out, Path::new("c.toml")).unwrap();
        assert_eq!(config.accounts.len(), 2);
        assert_eq!(config.accounts[1].name, "corp");
        assert_eq!(config.accounts[1].kind, AccountKind::M365);
        assert_eq!(config.accounts[1].tenant.as_deref(), Some("common"));
        assert_eq!(config.accounts[1].color.as_deref(), Some("magenta"));
        assert_eq!(config.accounts[1].client_secret, None);
    }

    #[test]
    fn append_to_empty_text_creates_valid_config() {
        let out = append_account("", Path::new("c.toml"), &account("corp", "me@corp.com")).unwrap();
        let config = parse(&out, Path::new("c.toml")).unwrap();
        assert_eq!(config.accounts.len(), 1);
    }

    #[test]
    fn append_rejects_unparseable_text() {
        let err =
            append_account("[[accounts", Path::new("c.toml"), &account("a", "a@b.c")).unwrap_err();
        assert!(matches!(err, ConfigError::Parse { .. }), "{err}");
    }

    #[test]
    fn set_secrets_keeps_comments_and_writes_the_onepassword_table() {
        let op = OnePasswordConfig {
            vault: "Rail".into(),
            account: Some("my.1password.com".into()),
            op_path: None,
        };
        let out = set_secrets(
            EXISTING,
            Path::new("c.toml"),
            SecretsBackend::OnePassword,
            Some(&op),
        )
        .unwrap();
        assert!(out.contains("# the good one"), "{out}");
        let config = parse(&out, Path::new("c.toml")).unwrap();
        assert_eq!(config.general.secrets, SecretsBackend::OnePassword);
        assert_eq!(config.general.editor.as_deref(), Some("nvim"));
        assert_eq!(config.onepassword, Some(op));
        assert_eq!(config.accounts.len(), 1);
        let back = set_secrets(&out, Path::new("c.toml"), SecretsBackend::File, None).unwrap();
        let config = parse(&back, Path::new("c.toml")).unwrap();
        assert_eq!(config.general.secrets, SecretsBackend::File);
        assert!(
            config.onepassword.is_some(),
            "the table is kept for switching back"
        );
    }

    #[test]
    fn set_secrets_updates_a_commented_onepassword_table_in_place() {
        let text = "[onepassword]\n# which vault\nvault = \"Old\" # trailing\nop_path = \"/usr/bin/op\"\n# keep me\n";
        let op = OnePasswordConfig {
            vault: "New".into(),
            account: Some("a.1password.com".into()),
            op_path: Some("/usr/bin/op".into()),
        };
        let out = set_secrets(
            text,
            Path::new("c.toml"),
            SecretsBackend::OnePassword,
            Some(&op),
        )
        .unwrap();
        for kept in ["# which vault", "# trailing", "# keep me", "/usr/bin/op"] {
            assert!(out.contains(kept), "{kept} lost:\n{out}");
        }
        let config = parse(&out, Path::new("c.toml")).unwrap();
        assert_eq!(config.onepassword, Some(op));
    }

    #[test]
    fn set_secrets_creates_the_general_table_and_refuses_1password_without_a_vault() {
        let out = set_secrets("", Path::new("c.toml"), SecretsBackend::File, None).unwrap();
        assert!(out.contains("secrets = \"file\""), "{out}");
        let err = set_secrets("", Path::new("c.toml"), SecretsBackend::OnePassword, None);
        assert!(matches!(err, Err(ConfigError::Invalid { .. })), "{err:?}");
    }

    #[test]
    fn append_writes_sync_overrides_only_when_set() {
        let plain = append_account("", Path::new("c.toml"), &account("a", "a@x.com")).unwrap();
        assert!(!plain.contains("history_days"), "{plain}");
        let tuned = AccountConfig {
            history_days: Some(2),
            body_cache_days: Some(1),
            ..account("b", "b@x.com")
        };
        let out = append_account("", Path::new("c.toml"), &tuned).unwrap();
        assert!(out.contains("history_days = 2"), "{out}");
        assert!(out.contains("body_cache_days = 1"), "{out}");
        let config = parse(&out, Path::new("c.toml")).unwrap();
        assert_eq!(config.accounts[0].history_days, Some(2));
    }

    #[test]
    fn append_writes_from_name_only_when_set() {
        let unset = append_account("", Path::new("c.toml"), &account("a", "a@x.com")).unwrap();
        assert!(!unset.contains("from_name"), "{unset}");
        let named = AccountConfig {
            from_name: Some("Dana Reyes".into()),
            ..account("b", "b@x.com")
        };
        let out = append_account("", Path::new("c.toml"), &named).unwrap();
        let config = parse(&out, Path::new("c.toml")).unwrap();
        assert_eq!(config.accounts[0].from_name.as_deref(), Some("Dana Reyes"));
    }

    #[test]
    fn append_writes_ai_send_only_when_set() {
        let unset = append_account("", Path::new("c.toml"), &account("a", "a@x.com")).unwrap();
        assert!(!unset.contains("ai_send"), "{unset}");
        let drafts = AccountConfig {
            ai_send: Some(crate::config::AiSend::Drafts),
            ..account("b", "b@x.com")
        };
        let out = append_account("", Path::new("c.toml"), &drafts).unwrap();
        assert!(out.contains("ai_send = \"drafts\""), "{out}");
        let config = parse(&out, Path::new("c.toml")).unwrap();
        assert_eq!(config.accounts[0].ai_send(), crate::config::AiSend::Drafts);
    }

    #[test]
    fn remove_account_keeps_comments_and_the_other_accounts() {
        let two = append_account(
            EXISTING,
            Path::new("c.toml"),
            &account("corp", "me@corp.com"),
        )
        .unwrap();
        let out = remove_account(&two, Path::new("c.toml"), "work").unwrap();
        assert!(out.contains("# my rail config"), "{out}");
        assert!(out.contains("# the good one"), "{out}");
        let config = parse(&out, Path::new("c.toml")).unwrap();
        let names: Vec<_> = config.accounts.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, ["corp"]);
        let last = remove_account(&out, Path::new("c.toml"), "corp").unwrap();
        assert!(
            parse(&last, Path::new("c.toml"))
                .unwrap()
                .accounts
                .is_empty()
        );
        let err = remove_account(&last, Path::new("c.toml"), "corp").unwrap_err();
        assert!(err.to_string().contains("no [[accounts]] entry"), "{err}");
    }

    #[test]
    fn default_name_is_local_part_made_unique() {
        let mut config = Config::default();
        assert_eq!(
            default_account_name("Dana.Reyes+x@corp.com", &config),
            "dana_reyes_x"
        );
        config
            .accounts
            .push(account("dana_reyes_x", "other@corp.com"));
        assert_eq!(
            default_account_name("dana.reyes+x@corp.com", &config),
            "dana_reyes_x-2"
        );
    }

    #[test]
    fn set_general_changes_one_key_and_keeps_everything_else() {
        let text = "# my rail config\n[general]\neditor = \"nvim\" # the good one\nicons = \"ascii\"\n\n[[accounts]]\nname = \"work\"\nkind = \"gmail\"\naddress = \"me@example.com\"\nclient_id = \"g\"\n";
        let path = Path::new("c.toml");
        let out = set_general(text, path, "theme", Some("Dracula".into())).unwrap();
        let expected = text.replace(
            "icons = \"ascii\"\n",
            "icons = \"ascii\"\ntheme = \"Dracula\"\n",
        );
        assert_eq!(out, expected);
        let config = parse(&out, path).unwrap();
        assert_eq!(config.general.theme.as_deref(), Some("Dracula"));

        let again = set_general(&out, path, "theme", Some("Nord".into())).unwrap();
        assert_eq!(again, out.replace("\"Dracula\"", "\"Nord\""));

        let slot = set_general(&again, path, "accent", Some(5i64.into())).unwrap();
        assert!(slot.contains("accent = 5\n"), "{slot}");

        let removed = set_general(&slot, path, "theme", None).unwrap();
        assert!(!removed.contains("theme"), "{removed}");
        assert!(removed.contains("# the good one"), "{removed}");
        assert_eq!(
            set_general(&removed, path, "theme", None).unwrap(),
            removed,
            "removing an absent key changes nothing"
        );
    }

    #[test]
    fn set_general_creates_the_table_and_rejects_a_value_the_config_refuses() {
        let out = set_general("", Path::new("c.toml"), "theme", Some("Nord".into())).unwrap();
        assert_eq!(out, "[general]\ntheme = \"Nord\"\n");
        let err = set_general("", Path::new("c.toml"), "accent", Some(99i64.into()));
        assert!(matches!(err, Err(ConfigError::Parse { .. })), "{err:?}");
    }

    #[test]
    fn the_text_and_number_wrappers_round_trip() {
        let path = Path::new("c.toml");
        let out = set_general_text("", path, "theme", Some("Nord")).unwrap();
        assert_eq!(out, "[general]\ntheme = \"Nord\"\n");
        assert_eq!(
            set_general_text(&out, path, "theme", None).unwrap(),
            "[general]\n"
        );
        let out = set_general_number("", path, "accent", Some(7)).unwrap();
        assert_eq!(out, "[general]\naccent = 7\n");
        assert_eq!(
            set_general_number(&out, path, "accent", None).unwrap(),
            "[general]\n"
        );
    }

    #[test]
    fn set_general_bool_writes_false_keeps_the_rest_and_none_removes_it() {
        let text =
            "# my rail config\n[general]\neditor = \"nvim\" # the good one\nicons = \"ascii\"\n";
        let path = Path::new("c.toml");
        let out = set_general_bool(text, path, "hints", Some(false)).unwrap();
        assert_eq!(out, format!("{text}hints = false\n"));
        let config = parse(&out, path).unwrap();
        assert!(!config.general.hints);
        assert_eq!(config.general.editor.as_deref(), Some("nvim"));
        let removed = set_general_bool(&out, path, "hints", None).unwrap();
        assert_eq!(removed, text, "the key goes and every comment stays");
        assert!(parse(&removed, path).unwrap().general.hints);
    }

    #[test]
    fn set_general_text_saves_a_spaced_theme_and_refuses_control_and_format_characters() {
        let path = Path::new("c.toml");
        let out = set_general_text("", path, "theme", Some("Catppuccin Mocha")).unwrap();
        assert_eq!(
            parse(&out, path).unwrap().general.theme.as_deref(),
            Some("Catppuccin Mocha")
        );
        for bad in ["a\tb", "a\u{202E}b"] {
            let err = set_general_text("", path, "theme", Some(bad)).unwrap_err();
            let message = err.to_string();
            assert!(message.contains("theme"), "{message}");
            assert!(
                !message.contains(bad.chars().nth(1).unwrap()),
                "{message:?}"
            );
        }
    }
}
