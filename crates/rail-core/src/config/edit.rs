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
    let decor = table
        .get(key)
        .and_then(Item::as_value)
        .map(|v| v.decor().clone());
    let mut new = toml_edit::Value::from(text);
    if let Some(decor) = decor {
        *new.decor_mut() = decor;
    }
    table[key] = Item::Value(new);
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
    accounts.push(table);

    let out = doc.to_string();
    parse(&out, path)?;
    Ok(out)
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
}
