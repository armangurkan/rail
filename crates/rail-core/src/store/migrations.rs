use rusqlite::{Connection, TransactionBehavior};

use super::StoreError;

const MIGRATIONS: &[(i64, &str)] = &[
    (1, include_str!("migrations/0001_init.sql")),
    (2, include_str!("migrations/0002_sync.sql")),
];

pub const LATEST_VERSION: i64 = 2;

/// Apply every migration newer than the database's `user_version`, each in its own transaction.
pub fn migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let current: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if current > LATEST_VERSION {
        return Err(StoreError::TooNew {
            found: current,
            supported: LATEST_VERSION,
        });
    }
    for (version, sql) in MIGRATIONS {
        // Take the write lock before reading `user_version`, so a concurrent opener
        // waits here and then sees the version we committed instead of re-applying.
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current: i64 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if current > LATEST_VERSION {
            return Err(StoreError::TooNew {
                found: current,
                supported: LATEST_VERSION,
            });
        }
        if *version <= current {
            continue;
        }
        tracing::info!(version, "applying store migration");
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}
