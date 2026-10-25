//! The sidebar's count beside each saved search: the threads it finds that hold an unread
//! message, one query per search.
//!
//! This is the one thing saved searches cost on a large mailbox — `sidebar_counts` runs on
//! every refresh, and these queries have no `IN_INBOX` to narrow them. Three things bound it:
//! the cap of 20 searches, the `t.unread_count > 0` predicate placed **first** so the filter's
//! `EXISTS` only runs for threads that could count, and `[general] saved_search_counts =
//! false`, which makes the caller pass no filters at all and so run no queries.

use rusqlite::types::Value;

use super::filter::Filter;
use super::view::{AccountFilter, count};
use super::{Store, StoreError};

impl Store {
    /// One unread-thread count per `(filter, account)` pair, in that order, over the accounts
    /// `ids` (a JSON id list) — an id with no row in the store simply matches nothing.
    ///
    /// The `AccountFilter` is the saved query's own `account:`, already resolved by the caller:
    /// `Filter::message_sql` emits no account condition, so without it a saved `account:home`
    /// search would be counted across every account while its list showed one. `Nobody` counts
    /// 0 without running a query at all.
    ///
    /// # Errors
    /// The store could not be read.
    pub fn saved_counts(
        &self,
        ids: &str,
        filters: &[(Filter, AccountFilter)],
        utc_offset: i32,
    ) -> Result<Vec<u64>, StoreError> {
        let mut counts = Vec::with_capacity(filters.len());
        for (filter, account) in filters {
            counts.push(self.saved_count(ids, filter, *account, utc_offset)?);
        }
        Ok(counts)
    }

    /// One saved search's count. The unread predicate comes before the filter's `EXISTS`, as
    /// `starred_unread` puts its own cheap condition first.
    fn saved_count(
        &self,
        ids: &str,
        filter: &Filter,
        account: AccountFilter,
        utc_offset: i32,
    ) -> Result<u64, StoreError> {
        if account == AccountFilter::Nobody {
            return Ok(0);
        }
        let mut sql = "SELECT count(*) FROM threads t
             WHERE t.account_id IN (SELECT value FROM json_each(?1))
               AND t.unread_count > 0"
            .to_string();
        let mut params: Vec<Value> = vec![Value::Text(ids.to_string())];
        if let AccountFilter::Only(id) = account {
            sql.push_str(" AND t.account_id = ?");
            params.push(Value::Integer(id));
        }
        if let Some((clause, values)) = filter.message_sql(utc_offset) {
            sql.push_str(" AND ");
            sql.push_str(&clause);
            params.extend(values);
        }
        let n: i64 = self
            .conn
            .query_row(&sql, rusqlite::params_from_iter(params), |row| row.get(0))?;
        Ok(count(n))
    }
}
