use rusqlite::Connection;

use super::StoreError;

const MIGRATIONS: &[(i64, &str)] = &[(1, include_str!("migrations/0001_init.sql"))];

pub const LATEST_VERSION: i64 = 1;

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
        if *version <= current {
            continue;
        }
        tracing::info!(version, "applying store migration");
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
    }
    Ok(())
}
