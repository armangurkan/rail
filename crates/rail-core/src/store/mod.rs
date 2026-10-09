//! The local SQLite store. One file, WAL mode, FTS5.

pub mod migrations;

use std::path::{Path, PathBuf};

use rusqlite::{Connection, ErrorCode};

use crate::error::ErrorClass;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("cannot create {}: {source}", .path.display())]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        "database schema version {found} is newer than this build supports ({supported}); upgrade rail"
    )]
    TooNew { found: i64, supported: i64 },
}

impl StoreError {
    /// Busy or locked databases clear up on their own; everything else will not.
    pub fn class(&self) -> ErrorClass {
        match self {
            Self::Sqlite(e)
                if matches!(
                    e.sqlite_error_code(),
                    Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
                ) =>
            {
                ErrorClass::Transient
            }
            Self::Sqlite(_) | Self::CreateDir { .. } | Self::TooNew { .. } => ErrorClass::Permanent,
        }
    }
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Pre-create the file owner-only so it is never visible at umask mode.
#[cfg(unix)]
fn create_private_file(file: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(file)?;
    Ok(())
}

#[cfg(not(unix))]
fn create_private_file(_file: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_to_owner(file: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_to_owner(_file: &Path) -> std::io::Result<()> {
    Ok(())
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            create_private_dir(parent).map_err(|source| StoreError::CreateDir {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        create_private_file(path).map_err(|source| StoreError::CreateDir {
            path: path.to_path_buf(),
            source,
        })?;
        let conn = Connection::open(path)?;
        restrict_to_owner(path).map_err(|source| StoreError::CreateDir {
            path: path.to_path_buf(),
            source,
        })?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(mut conn: Connection) -> Result<Self, StoreError> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        migrations::migrate(&mut conn)?;
        Ok(Self { conn })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Hand the connection back; used by tests and by the Phase 2 writer actor.
    pub fn into_connection(self) -> Connection {
        self.conn
    }

    pub fn schema_version(&self) -> Result<i64, StoreError> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |r| r.get(0))?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;

    fn table_names(store: &Store) -> Vec<String> {
        let mut stmt = store
            .conn()
            .prepare("SELECT name FROM sqlite_master WHERE type IN ('table') AND name NOT LIKE 'sqlite_%' ORDER BY name")
            .unwrap();
        stmt.query_map([], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn fresh_file_store_is_migrated_and_wal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("rail.db");
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), migrations::LATEST_VERSION);
        let mode: String = store
            .conn()
            .pragma_query_value(None, "journal_mode", |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        let fk: i64 = store
            .conn()
            .pragma_query_value(None, "foreign_keys", |r| r.get(0))
            .unwrap();
        assert_eq!(fk, 1);
    }

    #[test]
    fn reopen_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rail.db");
        let first = Store::open(&path).unwrap();
        first
            .conn()
            .execute(
                "INSERT INTO accounts (name, kind, address) VALUES ('a', 'gmail', 'a@example.com')",
                [],
            )
            .unwrap();
        drop(first);
        let store = Store::open(&path).unwrap();
        assert_eq!(store.schema_version().unwrap(), migrations::LATEST_VERSION);
        let n: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM accounts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn schema_has_every_table_from_the_spec() {
        let store = Store::open_in_memory().unwrap();
        let names = table_names(&store);
        for expected in [
            "accounts",
            "folders",
            "threads",
            "messages",
            "message_folders",
            "attachments",
            "addresses",
            "outbox",
            "messages_fts",
        ] {
            assert!(
                names.iter().any(|n| n == expected),
                "missing {expected} in {names:?}"
            );
        }
    }

    #[test]
    fn newer_schema_is_refused() {
        let store = Store::open_in_memory().unwrap();
        store
            .conn()
            .pragma_update(None, "user_version", 999)
            .unwrap();
        let err = migrations::migrate(&mut store.into_connection()).unwrap_err();
        assert!(
            matches!(err, StoreError::TooNew { found: 999, .. }),
            "{err}"
        );
    }

    fn seed_message(store: &Store, subject: &str) -> i64 {
        let c = store.conn();
        c.execute(
            "INSERT INTO accounts (name, kind, address) VALUES ('a', 'gmail', 'a@example.com')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO threads (account_id, remote_id) VALUES (1, 't1')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO messages (account_id, thread_id, remote_id, subject, from_json, body_text)
             VALUES (1, 1, 'm1', ?1, '[{\"name\":\"Dana\",\"address\":\"dana@example.com\"}]', 'hello world')",
            params![subject],
        )
        .unwrap();
        c.last_insert_rowid()
    }

    fn fts_hits(store: &Store, query: &str) -> Vec<i64> {
        let mut stmt = store
            .conn()
            .prepare("SELECT rowid FROM messages_fts WHERE messages_fts MATCH ?1")
            .unwrap();
        stmt.query_map(params![query], |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn fts_follows_inserts_updates_and_deletes() {
        let store = Store::open_in_memory().unwrap();
        let id = seed_message(&store, "quarterly roadmap");
        assert_eq!(fts_hits(&store, "roadmap"), vec![id]);
        assert_eq!(fts_hits(&store, "dana"), vec![id]);
        assert_eq!(fts_hits(&store, "hello"), vec![id]);

        store
            .conn()
            .execute(
                "UPDATE messages SET subject = 'offsite plans' WHERE id = ?1",
                params![id],
            )
            .unwrap();
        assert!(fts_hits(&store, "roadmap").is_empty());
        assert_eq!(fts_hits(&store, "offsite"), vec![id]);

        store
            .conn()
            .execute("DELETE FROM messages WHERE id = ?1", params![id])
            .unwrap();
        assert!(fts_hits(&store, "offsite").is_empty());
    }

    #[test]
    fn deleting_an_account_cascades() {
        let store = Store::open_in_memory().unwrap();
        seed_message(&store, "x");
        store
            .conn()
            .execute("DELETE FROM accounts WHERE id = 1", [])
            .unwrap();
        let n: i64 = store
            .conn()
            .query_row("SELECT count(*) FROM messages", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn errors_are_classified_for_retry() {
        let too_new = StoreError::TooNew {
            found: 9,
            supported: 1,
        };
        assert_eq!(too_new.class(), ErrorClass::Permanent);

        let busy = StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_BUSY),
            None,
        ));
        assert_eq!(busy.class(), ErrorClass::Transient);

        let constraint = StoreError::Sqlite(rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT),
            None,
        ));
        assert_eq!(constraint.class(), ErrorClass::Permanent);
    }

    #[cfg(unix)]
    #[test]
    fn database_and_directory_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let data_dir = dir.path().join("data");
        let path = data_dir.join("rail.db");
        let store = Store::open(&path).unwrap();
        store
            .conn()
            .execute(
                "INSERT INTO accounts (name, kind, address) VALUES ('a', 'gmail', 'a@example.com')",
                [],
            )
            .unwrap();
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(&data_dir), 0o700);
        let wal = data_dir.join("rail.db-wal");
        assert!(wal.exists(), "expected a WAL file after a write");
        assert_eq!(mode(&wal), 0o600);
    }
}
