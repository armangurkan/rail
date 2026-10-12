//! The addresses mail has taught the store, for `To:` completion.

use rusqlite::{Transaction, params};

use super::{Store, StoreError};
use crate::provider::{Envelope, Mailbox};

/// Suggestions `address_suggestions` returns at most.
pub const MAX_SUGGESTIONS: usize = 20;

/// One suggestion. Both fields are outside text.
#[derive(Clone, PartialEq, Eq)]
pub struct AddressEntry {
    pub name: Option<String>,
    pub address: String,
}

impl std::fmt::Debug for AddressEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AddressEntry")
            .field("named", &self.name.is_some())
            .finish_non_exhaustive()
    }
}

/// Upsert the from, to and cc of `envelope`, keyed by the lower-cased address: hits + 1, the
/// latest non-empty name, last seen. Anything without an `@` is not an address and is skipped.
pub(super) fn remember(tx: &Transaction, envelope: &Envelope, now: i64) -> Result<(), StoreError> {
    let all = envelope.from.iter().chain(&envelope.to).chain(&envelope.cc);
    for Mailbox { name, address } in all {
        let key = address.trim().to_ascii_lowercase();
        if key.is_empty() || !key.contains('@') {
            continue;
        }
        let name = name.as_deref().map(str::trim).filter(|n| !n.is_empty());
        tx.execute(
            "INSERT INTO addresses (address, name, hits, last_seen) VALUES (?1, ?2, 1, ?3)
             ON CONFLICT(address) DO UPDATE SET
               hits = hits + 1,
               name = COALESCE(excluded.name, name),
               last_seen = max(last_seen, excluded.last_seen)",
            params![key, name, now],
        )?;
    }
    Ok(())
}

impl Store {
    /// The best matches for `prefix`: the address, its domain, or any word of the name starts
    /// with it (case-insensitive); an empty prefix lists everything. Most-hit first, then most
    /// recent; at most `limit`, never more than `MAX_SUGGESTIONS`.
    pub fn address_suggestions(
        &self,
        prefix: &str,
        limit: usize,
    ) -> Result<Vec<AddressEntry>, StoreError> {
        let limit = limit.min(MAX_SUGGESTIONS);
        let needle = prefix.trim().to_lowercase();
        let mut stmt = self.conn.prepare(
            "SELECT address, name FROM addresses ORDER BY hits DESC, last_seen DESC, address",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(AddressEntry {
                address: r.get(0)?,
                name: r.get(1)?,
            })
        })?;
        let mut out = Vec::new();
        for entry in rows {
            if out.len() >= limit {
                break;
            }
            let entry = entry?;
            if matches(&entry, &needle) {
                out.push(entry);
            }
        }
        Ok(out)
    }
}

fn matches(entry: &AddressEntry, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let address = entry.address.to_lowercase();
    if address.starts_with(needle) {
        return true;
    }
    if let Some((_, domain)) = address.split_once('@')
        && domain.starts_with(needle)
    {
        return true;
    }
    entry
        .name
        .as_deref()
        .map(str::to_lowercase)
        .is_some_and(|name| name.split_whitespace().any(|word| word.starts_with(needle)))
}
