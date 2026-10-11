//! `body.fetch`: bodies the store does not hold, fetched through the account's engine provider
//! (the same quota pacing, retries and token source) and committed through the writer. The
//! answer is one outcome per id; bodies never leave the daemon in it.

use std::collections::HashMap;

use futures_util::{StreamExt, stream};

use super::batch::Batch;
use super::manager::{Reach, SearchTarget, unobserved};
use super::{BODY_BATCH, BODY_BATCH_BYTES, BODY_CONCURRENCY, UNREADABLE_BODY};
use crate::accounts::blocking;
use crate::daemon::DaemonContext;
use crate::ops::{BodyFetchRequest, BodyFetchResponse, BodyFetchResult, BodyOutcome, OpError};
use crate::provider::{Body, ProviderError, RemoteId};
use crate::store::{Store, StoredBody};

pub async fn run(
    ctx: &DaemonContext,
    request: BodyFetchRequest,
) -> Result<BodyFetchResponse, OpError> {
    let ids = request.checked_ids()?;
    if ids.is_empty() {
        return Ok(BodyFetchResponse {
            results: Vec::new(),
        });
    }
    let target = match ctx.sync.body_target(&request.account).await? {
        Reach::Ready(target) => target,
        Reach::Unavailable(error) => {
            return Ok(answer(&ids, |_| BodyOutcome::Failed(error.clone())));
        }
    };
    // The writer opens (and migrates) the store; the handle is only taken to write.
    ctx.sync.writer(ctx).await?;
    let stored = stored_bodies(ctx, target.account_id, ids.clone()).await?;
    let wanted: Vec<RemoteId> = ids
        .iter()
        .filter(|id| stored.get(*id) == Some(&StoredBody::Pending))
        .cloned()
        .collect();
    let mut fetched = unobserved(fetch_all(ctx, &target, wanted)).await?;
    Ok(answer(&ids, |id| match stored.get(id) {
        None => BodyOutcome::Missing,
        Some(StoredBody::Stored) => BodyOutcome::Fetched,
        Some(StoredBody::Unreadable) => BodyOutcome::Unreadable,
        Some(StoredBody::Pending) => fetched.remove(id).unwrap_or_else(|| {
            BodyOutcome::Failed(OpError::internal("a body fetch reported nothing"))
        }),
    }))
}

fn answer(ids: &[String], mut outcome: impl FnMut(&str) -> BodyOutcome) -> BodyFetchResponse {
    BodyFetchResponse {
        results: ids
            .iter()
            .map(|id| BodyFetchResult {
                message_id: id.clone(),
                outcome: outcome(id),
            })
            .collect(),
    }
}

async fn stored_bodies(
    ctx: &DaemonContext,
    account_id: i64,
    ids: Vec<String>,
) -> Result<HashMap<String, StoredBody>, OpError> {
    let db = ctx.paths.db_file();
    blocking(move || Ok(Store::open_reader(&db)?.body_states(account_id, &ids)?)).await
}

/// Fetch `wanted`, at most `BODY_CONCURRENCY` at a time, writing every `BODY_BATCH` bodies
/// (or `BODY_BATCH_BYTES` of them) as they arrive; each id's outcome.
async fn fetch_all(
    ctx: &DaemonContext,
    target: &SearchTarget,
    wanted: Vec<RemoteId>,
) -> Result<HashMap<RemoteId, BodyOutcome>, OpError> {
    let provider = &target.provider;
    let mut fetches = stream::iter(wanted)
        .map(|id| async move {
            let result = provider.body(&id).await;
            (id, result)
        })
        .buffer_unordered(BODY_CONCURRENCY);
    let mut outcomes = HashMap::new();
    let mut pending: Vec<(RemoteId, Body)> = Vec::new();
    let mut bytes = 0usize;
    while let Some((id, result)) = fetches.next().await {
        let (outcome, body) = sort(&target.name, &id, result);
        if let Some(body) = body {
            bytes =
                bytes.saturating_add(body.text.len() + body.html.as_ref().map_or(0, String::len));
            pending.push((id.clone(), body));
        }
        outcomes.insert(id, outcome);
        if pending.len() >= BODY_BATCH || bytes >= BODY_BATCH_BYTES {
            write(ctx, target.account_id, std::mem::take(&mut pending)).await?;
            bytes = 0;
        }
    }
    write(ctx, target.account_id, pending).await?;
    Ok(outcomes)
}

/// The outcome of one fetch, and the body to store for it.
fn sort(
    account: &str,
    id: &str,
    result: Result<Body, ProviderError>,
) -> (BodyOutcome, Option<Body>) {
    match result {
        Ok(body) => (BodyOutcome::Fetched, Some(body)),
        Err(ProviderError::NotFound) => (BodyOutcome::Missing, None),
        Err(ProviderError::Permanent { .. }) => {
            tracing::warn!(account, id = %id.escape_debug(), "stored a marker for a body Rail could not read");
            let marker = Body {
                text: UNREADABLE_BODY.to_string(),
                ..Body::default()
            };
            (BodyOutcome::Unreadable, Some(marker))
        }
        Err(error) => {
            tracing::warn!(account, id = %id.escape_debug(), code = error.code(), "a body fetch failed");
            (BodyOutcome::Failed(error.into()), None)
        }
    }
}

async fn write(
    ctx: &DaemonContext,
    account_id: i64,
    bodies: Vec<(RemoteId, Body)>,
) -> Result<(), OpError> {
    if bodies.is_empty() {
        return Ok(());
    }
    let batch = Batch {
        bodies,
        ..Batch::new(account_id)
    };
    ctx.sync.writer(ctx).await?.write(batch).await?;
    Ok(())
}
