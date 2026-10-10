use rusqlite::{params, types::Type};

use super::{Store, StoreError};
use crate::config::{AccountConfig, AccountKind, Config};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountRow {
    pub id: i64,
    pub name: String,
    pub kind: AccountKind,
    pub address: String,
    pub color: Option<String>,
    pub sync_cursor: Option<String>,
    pub last_sync_at: Option<i64>,
}

impl Store {
    /// Insert or update by address. Addresses are stored lowercase.
    pub fn upsert_account(&self, account: &AccountConfig) -> Result<i64, StoreError> {
        let address = account.address.to_ascii_lowercase();
        self.conn.execute(
            "INSERT INTO accounts (name, kind, address, color) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(address) DO UPDATE SET
               name = excluded.name, kind = excluded.kind, color = excluded.color",
            params![account.name, account.kind.as_str(), address, account.color],
        )?;
        let id = self.conn.query_row(
            "SELECT id FROM accounts WHERE address = ?1",
            params![address],
            |r| r.get(0),
        )?;
        Ok(id)
    }

    pub fn list_accounts(&self) -> Result<Vec<AccountRow>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, kind, address, color, sync_cursor, last_sync_at
             FROM accounts ORDER BY id",
        )?;
        let rows = stmt.query_map([], |r| {
            let kind: String = r.get(2)?;
            let kind = AccountKind::parse(&kind)
                .ok_or_else(|| rusqlite::Error::InvalidColumnType(2, "kind".into(), Type::Text))?;
            Ok(AccountRow {
                id: r.get(0)?,
                name: r.get(1)?,
                kind,
                address: r.get(3)?,
                color: r.get(4)?,
                sync_cursor: r.get(5)?,
                last_sync_at: r.get(6)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Make the store reflect every account in config. Rows for accounts no longer in
    /// config are left alone: Rail never deletes mail implicitly.
    pub fn sync_accounts_from_config(&self, config: &Config) -> Result<Vec<i64>, StoreError> {
        config
            .accounts
            .iter()
            .map(|a| self.upsert_account(a))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AccountConfig, AccountKind, Config};
    use crate::test_support::sample_account;

    fn account(name: &str, address: &str) -> AccountConfig {
        AccountConfig {
            name: name.to_string(),
            address: address.to_string(),
            ..sample_account()
        }
    }

    #[test]
    fn upsert_inserts_then_updates_by_address() {
        let store = Store::open_in_memory().unwrap();
        let id1 = store
            .upsert_account(&account("work", "Me@Example.com"))
            .unwrap();
        let id2 = store
            .upsert_account(&account("job", "me@example.com"))
            .unwrap();
        assert_eq!(id1, id2);
        let rows = store.list_accounts().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "job");
        assert_eq!(rows[0].address, "me@example.com");
        assert_eq!(rows[0].kind, AccountKind::Gmail);
        assert_eq!(rows[0].color.as_deref(), Some("blue"));
        assert_eq!(rows[0].sync_cursor, None);
    }

    #[test]
    fn sync_from_config_upserts_every_account_and_keeps_unlisted_rows() {
        let store = Store::open_in_memory().unwrap();
        store
            .upsert_account(&account("old", "old@example.com"))
            .unwrap();
        let config = Config {
            accounts: vec![account("a", "a@example.com"), account("b", "b@example.com")],
            ..Config::default()
        };
        let ids = store.sync_accounts_from_config(&config).unwrap();
        assert_eq!(ids.len(), 2);
        let names: Vec<String> = store
            .list_accounts()
            .unwrap()
            .into_iter()
            .map(|r| r.name)
            .collect();
        assert_eq!(names, vec!["old", "a", "b"]);
    }

    #[test]
    fn a_name_can_be_reused_by_a_different_address() {
        let store = Store::open_in_memory().unwrap();
        store.upsert_account(&account("me", "me@a.com")).unwrap();
        // me@a.com was removed from config; a new account takes the same default name.
        store.upsert_account(&account("me", "me@b.com")).unwrap();
        let rows = store.list_accounts().unwrap();
        let pairs: Vec<(&str, &str)> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.address.as_str()))
            .collect();
        assert_eq!(pairs, vec![("me", "me@a.com"), ("me", "me@b.com")]);
    }

    #[test]
    fn swapping_names_in_config_syncs_cleanly() {
        let store = Store::open_in_memory().unwrap();
        let before = Config {
            accounts: vec![
                account("work", "a@example.com"),
                account("home", "b@example.com"),
            ],
            ..Config::default()
        };
        store.sync_accounts_from_config(&before).unwrap();
        let after = Config {
            accounts: vec![
                account("home", "a@example.com"),
                account("work", "b@example.com"),
            ],
            ..Config::default()
        };
        store.sync_accounts_from_config(&after).unwrap();
        let pairs: Vec<(String, String)> = store
            .list_accounts()
            .unwrap()
            .into_iter()
            .map(|r| (r.name, r.address))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("home".to_string(), "a@example.com".to_string()),
                ("work".to_string(), "b@example.com".to_string())
            ]
        );
    }
}
