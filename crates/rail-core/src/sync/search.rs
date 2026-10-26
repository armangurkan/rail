//! `search`: the local FTS query and each account's server query. The local query
//! runs to the end first, so a server hit written meanwhile can never pass for a local one.
//! Server ids missing locally are fetched with their metadata and stored with
//! `origin = search`; ids already stored refresh `searched_at`. Results merge newest first,
//! local first on ties, one per thread.

use std::cmp::Reverse;
use std::collections::HashSet;
use std::path::PathBuf;

use futures_util::future::join_all;

use super::batch::{Batch, Origin};
use super::manager::SearchTarget;
use crate::accounts::{blocking, find_account};
use crate::config;
use crate::daemon::DaemonContext;
use crate::ops::{
    HitSource, MAX_QUERY_CHARS, MAX_SEARCH_LIMIT, OpError, SearchFailure, SearchHit, SearchRequest,
    SearchResponse, Untrusted,
};
use crate::provider::http::{RetryPolicy, with_retry_policy};
use crate::provider::{EnvelopePage, Mailbox, RemoteId};
use crate::store::{Store, StoredHit};

pub async fn run(ctx: &DaemonContext, request: SearchRequest) -> Result<SearchResponse, OpError> {
    let query = checked_query(&request.query)?;
    let limit = request.limit.clamp(1, MAX_SEARCH_LIMIT);
    // The writer opens (and migrates) the store, so the readers below never meet a missing
    // database. The handle is not kept: shutdown waits for every handle to go. Without a
    // writer (shutting down, or given up) an existing store is still searched locally.
    let db = ctx.paths.db_file();
    if let Err(error) = ctx.sync.writer(ctx).await
        && !db.exists()
    {
        return Err(error);
    }
    let (accounts, address) = local_accounts(ctx, request.account.clone(), db.clone()).await?;
    let local = local_hits(db.clone(), query.clone(), accounts, limit).await?;
    // Accounts the server is not asked for (no engine, Microsoft 365, the writer gone) are
    // failures, so the front end can say whose mail was searched only locally.
    let (targets, mut failures) = ctx.sync.search_plan(address.as_deref()).await;
    // A search's retries are not a sync's: they leave each account's phase alone, and a
    // person is waiting, so one short retry (spec §7) instead of the sync's backoff.
    let servers = with_retry_policy(
        RetryPolicy::interactive(),
        super::manager::unobserved(join_all(
            targets
                .iter()
                .map(|t| server_hits(ctx, &db, t, &query, limit)),
        )),
    )
    .await;
    let mut found = Vec::new();
    for (target, outcome) in targets.iter().zip(servers) {
        match outcome {
            Ok(hits) => found.extend(hits),
            Err(error) => failures.push(SearchFailure {
                account: target.name.clone(),
                kind: target.kind,
                error,
            }),
        }
    }
    Ok(SearchResponse {
        hits: merge(local, found, limit),
        failures,
    })
}

fn checked_query(query: &str) -> Result<String, OpError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(OpError::invalid_input("the search query is empty"));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(OpError::invalid_input(format!(
            "the search query is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    Ok(query.to_string())
}

/// The store ids searched locally, and the address of the account `who` names: that one
/// account, or every account configured now. A removed account's mail stays in the store but
/// is never shown, even when a new account reuses its name.
async fn local_accounts(
    ctx: &DaemonContext,
    who: Option<String>,
    db: PathBuf,
) -> Result<(Vec<i64>, Option<String>), OpError> {
    let path = ctx.paths.config_file();
    blocking(move || {
        let config = match config::load(&path) {
            Err(config::ConfigError::Missing { .. }) => config::Config::default(),
            loaded => loaded?,
        };
        let store = Store::open_reader(&db)?;
        let Some(who) = who else {
            let mut ids = Vec::new();
            for account in &config.accounts {
                ids.extend(store.account_id(&account.address.to_ascii_lowercase())?);
            }
            return Ok((ids, None));
        };
        let address = find_account(&config, &who)?.address.to_ascii_lowercase();
        // An account the store has no row for yet matches nothing locally.
        let ids = store.account_id(&address)?.into_iter().collect();
        Ok((ids, Some(address)))
    })
    .await
}

async fn local_hits(
    db: PathBuf,
    query: String,
    accounts: Vec<i64>,
    limit: usize,
) -> Result<Vec<StoredHit>, OpError> {
    // Several messages of one thread can match: ask for more than `limit` rows.
    blocking(move || {
        let store = Store::open_reader(&db)?;
        Ok(store.search_local(&query, Some(&accounts), limit * 4)?)
    })
    .await
}

async fn server_hits(
    ctx: &DaemonContext,
    db: &std::path::Path,
    target: &SearchTarget,
    query: &str,
    limit: usize,
) -> Result<Vec<StoredHit>, OpError> {
    let ids = target.provider.search(query, limit).await?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let (path, id, all) = (db.to_path_buf(), target.account_id, ids.clone());
    let known =
        blocking(move || Ok(Store::open_reader(&path)?.known_remote_ids(id, &all)?)).await?;
    let missing: Vec<RemoteId> = ids
        .iter()
        .filter(|i| !known.contains(*i))
        .cloned()
        .collect();
    let fetched = if missing.is_empty() {
        EnvelopePage::default()
    } else {
        target.provider.envelopes(&missing).await?
    };
    let batch = Batch {
        upserts: fetched.envelopes,
        origin: Origin::Search,
        searched: known.into_iter().collect(),
        ..Batch::new(target.account_id)
    };
    let outcome = ctx.sync.writer(ctx).await?.write(batch).await?;
    // Settled failures change the outbox depth; mail that made snoozes due needs the worker.
    if outcome.failed_cleared > 0 || outcome.snoozes_due > 0 {
        ctx.sync.outbox_changed(ctx).await;
    }
    let path = db.to_path_buf();
    blocking(move || Ok(Store::open_reader(&path)?.hits_for(id, &ids)?)).await
}

fn merge(local: Vec<StoredHit>, server: Vec<StoredHit>, limit: usize) -> Vec<SearchHit> {
    let mut all: Vec<(StoredHit, HitSource)> = local
        .into_iter()
        .map(|h| (h, HitSource::Local))
        .chain(server.into_iter().map(|h| (h, HitSource::Server)))
        .collect();
    // Stable: on equal dates the local hit, which came first, stays first.
    all.sort_by_key(|(hit, _)| Reverse(hit.internal_date));
    let mut seen = HashSet::new();
    all.into_iter()
        .filter(|(hit, _)| seen.insert((hit.account.clone(), hit.thread_remote_id.clone())))
        .take(limit)
        .map(|(hit, source)| SearchHit {
            account: hit.account,
            thread_id: hit.thread_remote_id,
            date: hit.internal_date.div_euclid(1000),
            sender: Untrusted::new(sender(&hit.from_json)),
            subject: Untrusted::new(hit.subject),
            source,
        })
        .collect()
}

/// `Name <address>` of the first sender, or the address alone.
fn sender(from_json: &str) -> String {
    let senders: Vec<Mailbox> = serde_json::from_str(from_json).unwrap_or_default();
    match senders.first() {
        Some(Mailbox {
            name: Some(name),
            address,
        }) if !name.is_empty() => format!("{name} <{address}>"),
        Some(first) => first.address.clone(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(account: &str, thread: &str, date: i64) -> StoredHit {
        StoredHit {
            account: account.into(),
            remote_id: format!("{thread}-m"),
            thread_remote_id: thread.into(),
            subject: "s".into(),
            from_json: "[]".into(),
            internal_date: date,
            origin: "sync".into(),
        }
    }

    /// Newest first, local first on equal dates, one per thread, at most `limit`.
    #[test]
    fn merge_orders_by_date_with_local_first_on_ties() {
        let local = vec![hit("w", "a", 2_000), hit("w", "b", 1_000)];
        let server = vec![
            hit("w", "c", 2_000),
            hit("w", "a", 3_000),
            hit("w", "d", 500),
        ];
        let merged = merge(local, server, 3);
        let got: Vec<(&str, HitSource)> = merged
            .iter()
            .map(|h| (h.thread_id.as_str(), h.source))
            .collect();
        assert_eq!(
            got,
            [
                ("a", HitSource::Server),
                ("c", HitSource::Server),
                ("b", HitSource::Local)
            ]
        );
        assert_eq!(merged[0].date, 3);
        let tie = merge(vec![hit("w", "x", 9)], vec![hit("w", "y", 9)], 10);
        assert_eq!(tie[0].source, HitSource::Local);
    }

    #[test]
    fn the_sender_is_the_first_mailbox() {
        assert_eq!(
            sender(r#"[{"name":"Dana","address":"d@x.com"},{"address":"e@x.com"}]"#),
            "Dana <d@x.com>"
        );
        assert_eq!(sender(r#"[{"name":"","address":"d@x.com"}]"#), "d@x.com");
        assert_eq!(sender("not json"), "");
    }

    #[test]
    fn a_query_must_have_words_and_a_bounded_length() {
        assert!(checked_query("  ").is_err());
        assert!(checked_query(&"x".repeat(MAX_QUERY_CHARS + 1)).is_err());
        assert_eq!(checked_query(" budget ").unwrap(), "budget");
    }
}
