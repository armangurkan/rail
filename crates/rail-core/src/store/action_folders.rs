//! Stable identities for folders explicitly named by queued actions, including no-op actions
//! with empty reversals. Rowids are resolved again for local replay and provider planning.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::outbox::OutboxRow;
use super::targets::folder_remote_in;
use super::{Store, StoreError};
use crate::ops::Action;

pub(super) fn capture(
    conn: &Connection,
    account: i64,
    action: &Action,
) -> Result<BTreeMap<i64, String>, StoreError> {
    let mut remotes = BTreeMap::new();
    for id in action.label_ids() {
        if let Some(remote) = folder_remote_in(conn, account, id)? {
            remotes.insert(id, remote);
        }
    }
    Ok(remotes)
}

fn resolve(conn: &Connection, row: &OutboxRow, id: i64) -> Result<Option<i64>, StoreError> {
    let remote = row.folder_remotes.get(&id);
    if let Some(remote) = remote {
        return Ok(conn
            .query_row(
                "SELECT id FROM folders WHERE account_id = ?1 AND remote_id = ?2",
                params![row.account_id, remote],
                |r| r.get(0),
            )
            .optional()?);
    }
    Ok(None)
}

fn resolve_ids(
    conn: &Connection,
    row: &OutboxRow,
    ids: &[i64],
) -> Result<Option<Vec<i64>>, StoreError> {
    let mut resolved = Vec::with_capacity(ids.len());
    for &id in ids {
        let Some(id) = resolve(conn, row, id)? else {
            return Ok(None);
        };
        resolved.push(id);
    }
    Ok(Some(resolved))
}

pub(super) fn action(conn: &Connection, row: &OutboxRow) -> Result<Option<Action>, StoreError> {
    Ok(match &row.action {
        Action::Labels { add, remove } => match (
            resolve_ids(conn, row, add)?,
            resolve_ids(conn, row, remove)?,
        ) {
            (Some(add), Some(remove)) => Some(Action::Labels { add, remove }),
            _ => None,
        },
        Action::Move { to } => resolve(conn, row, *to)?.map(|to| Action::Move { to }),
        action => Some(action.clone()),
    })
}

impl Store {
    pub(crate) fn row_action(&self, row: &OutboxRow) -> Result<Option<Action>, StoreError> {
        action(&self.conn, row)
    }
}
