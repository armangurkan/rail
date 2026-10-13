//! JSON batching: up to `MAX_BATCH` requests in one POST to `$batch`, each asking for
//! immutable ids. Answers are matched to requests by id (Graph may answer out of order). A
//! throttled item (a 429 inside the batch's 200) waits for its own `Retry-After` (at most
//! `MAX_ITEM_RETRY_AFTER`; a second without one) and is sent again on its own, through the
//! client's retries; the other items stand.

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use serde_json::{Map, Value, json};

use super::errors::{Refusal, refusal};
use super::wire::{BatchAnswers, BatchItem};
use super::{COST_REQUEST, GraphProvider};
use crate::provider::ProviderError;
use crate::provider::http::{IMMUTABLE_IDS, parse_retry_after};

/// Graph's limit of requests in one batch.
pub const MAX_BATCH: usize = 20;
/// The longest a throttled batch item waits before it is sent again alone.
pub const MAX_ITEM_RETRY_AFTER: Duration = Duration::from_secs(30);
/// How long a throttled item that names no wait waits.
pub(crate) const ITEM_RETRY_DEFAULT: Duration = Duration::from_secs(1);

/// How long a throttled item asks Rail to wait: its `Retry-After` (any case), capped at
/// `MAX_ITEM_RETRY_AFTER`; `ITEM_RETRY_DEFAULT` when it names none Rail can read.
pub(crate) fn item_wait(headers: &Map<String, Value>) -> Duration {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.as_str())
        .and_then(|value| parse_retry_after(value, SystemTime::now()))
        .map_or(ITEM_RETRY_DEFAULT, |wait| wait.min(MAX_ITEM_RETRY_AFTER))
}

/// One request of a batch; `url` is relative to the API root and starts with `/`.
pub(crate) struct BatchRequest {
    pub method: reqwest::Method,
    pub url: String,
    pub body: Option<Value>,
}

impl BatchRequest {
    pub(crate) fn patch(url: String, body: Value) -> Self {
        Self {
            method: reqwest::Method::PATCH,
            url,
            body: Some(body),
        }
    }

    pub(crate) fn post(url: String, body: Value) -> Self {
        Self {
            method: reqwest::Method::POST,
            url,
            body: Some(body),
        }
    }

    pub(crate) fn get(url: String) -> Self {
        Self {
            method: reqwest::Method::GET,
            url,
            body: None,
        }
    }
}

/// One request's answer.
pub(crate) struct BatchAnswer {
    /// The HTTP status; 0 for a resend refused for good, whose reading is in `refused`.
    pub status: u16,
    pub body: Option<Value>,
    /// A resend alone that Graph refused for good, already read as Rail's error.
    pub refused: Option<ProviderError>,
}

/// The body is message JSON: only the status is shown.
impl std::fmt::Debug for BatchAnswer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BatchAnswer")
            .field("status", &self.status)
            .finish_non_exhaustive()
    }
}

impl BatchAnswer {
    /// The body when the request succeeded, `None` when what it asked for is gone (404), else
    /// the refusal as Rail reads it (a 401 inside a batch means a new login).
    pub(crate) fn into_body(self) -> Result<Option<Value>, ProviderError> {
        if let Some(error) = self.refused {
            return Err(error);
        }
        match self.status {
            200..=299 => Ok(Some(self.body.unwrap_or(Value::Null))),
            404 => Ok(None),
            status => {
                let bytes = self
                    .body
                    .map(|body| body.to_string().into_bytes())
                    .unwrap_or_default();
                Err(match refusal(status, None, &bytes) {
                    Refusal::Unauthorized(detail) => ProviderError::NeedsAuth { detail },
                    Refusal::Failed(error) => error,
                })
            }
        }
    }
}

fn unreadable() -> ProviderError {
    ProviderError::permanent("Microsoft 365's batch answer was unreadable")
}

fn sub_request(index: usize, request: &BatchRequest) -> Value {
    let mut item = json!({
        "id": index.to_string(),
        "method": request.method.as_str(),
        "url": request.url,
        "headers": {"Prefer": IMMUTABLE_IDS},
    });
    if let Some(body) = &request.body {
        item["body"] = body.clone();
        item["headers"]["Content-Type"] = json!("application/json");
    }
    item
}

impl GraphProvider {
    /// Every request's answer, in the order asked.
    pub(crate) async fn batch(
        &self,
        requests: &[BatchRequest],
    ) -> Result<Vec<BatchAnswer>, ProviderError> {
        let mut answers = Vec::with_capacity(requests.len());
        for chunk in requests.chunks(MAX_BATCH) {
            answers.extend(self.batch_chunk(chunk).await?);
        }
        Ok(answers)
    }

    async fn batch_chunk(&self, chunk: &[BatchRequest]) -> Result<Vec<BatchAnswer>, ProviderError> {
        let items: Vec<Value> = chunk
            .iter()
            .enumerate()
            .map(|(index, request)| sub_request(index, request))
            .collect();
        let cost = u32::try_from(chunk.len()).unwrap_or(u32::MAX);
        let posted = self
            .api
            .post_batch(&json!({"requests": items}), cost)
            .await?;
        let parsed: BatchAnswers =
            serde_json::from_slice(&posted.ok_or_else(unreadable)?).map_err(|_| unreadable())?;
        let mut by_id: HashMap<String, BatchItem> = parsed
            .responses
            .into_iter()
            .map(|item| (item.id.clone(), item))
            .collect();
        let mut slots = Vec::with_capacity(chunk.len());
        let mut wait = None;
        for index in 0..chunk.len() {
            let item = by_id.remove(&index.to_string()).ok_or_else(unreadable)?;
            if item.status == 429 {
                wait = wait.max(Some(item_wait(&item.headers)));
                slots.push(None);
            } else {
                slots.push(Some(BatchAnswer {
                    status: item.status,
                    body: item.body,
                    refused: None,
                }));
            }
        }
        // One wait for the chunk's throttled items: the longest any asks, never past the cap.
        if let Some(wait) = wait {
            tokio::time::sleep(wait).await;
        }
        let mut answers = Vec::with_capacity(chunk.len());
        for (slot, request) in slots.into_iter().zip(chunk) {
            answers.push(match slot {
                Some(answer) => answer,
                None => self.alone(request).await?,
            });
        }
        Ok(answers)
    }

    /// `request` on its own, with the client's retries: a throttled batch item. A permanent
    /// refusal is that item's answer, for the caller to settle per message; a login or a
    /// transient failure fails the call.
    async fn alone(&self, request: &BatchRequest) -> Result<BatchAnswer, ProviderError> {
        let path = request.url.trim_start_matches('/');
        let method = request.method.clone();
        let sent = self
            .api
            .request_bytes(method, path, request.body.as_ref(), COST_REQUEST, true)
            .await;
        match sent {
            Ok(bytes) => Ok(BatchAnswer {
                status: 200,
                body: bytes.and_then(|b| serde_json::from_slice(&b).ok()),
                refused: None,
            }),
            Err(ProviderError::NotFound) => Ok(BatchAnswer {
                status: 404,
                body: None,
                refused: None,
            }),
            Err(error @ ProviderError::Permanent { .. }) => Ok(BatchAnswer {
                status: 0,
                body: None,
                refused: Some(error),
            }),
            Err(error) => Err(error),
        }
    }
}
