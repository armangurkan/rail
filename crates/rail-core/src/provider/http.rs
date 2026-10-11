//! The HTTP layer providers share: bearer tokens, a quota pacer, retries on 429, 5xx and
//! rate-limit 403s with capped exponential backoff and jitter (honouring `Retry-After`), one
//! token refresh and one retry on 401, and capped response reads. Requests are logged with
//! method, path, status and duration at debug: never the query string, a body or a token.

use std::sync::Arc;
use std::time::{Duration, Instant};

use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::ProviderError;
use super::tokens::AccessTokens;

/// The largest answer Rail reads; a `format=full` message of Gmail's 25 MB limit in base64
/// JSON fits.
pub const MAX_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
/// Quota units a minute one account may spend: below Gmail's 6,000 per user.
pub const QUOTA_PER_MINUTE: u32 = 5_000;
/// Units an account may spend at once before the pacing starts.
pub const QUOTA_BURST: u32 = 250;
/// The longest Gmail's own error text is kept.
const MAX_ERROR_TEXT: usize = 500;
/// The most of an error answer (non-2xx) Rail reads; the rest is never read.
const MAX_ERROR_BODY: usize = 64 * 1024;

/// Told how long the layer waits before each retry (the status shows "retrying in Ns").
pub type RetryObserver = Arc<dyn Fn(Duration) + Send + Sync>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Tries in all, the first included.
    pub attempts: u32,
    pub base: Duration,
    pub cap: Duration,
    pub max_retry_after: Duration,
}

impl Default for RetryPolicy {
    /// Gmail's guidance: 2^n seconds plus up to a second of jitter, capped at 32 s.
    fn default() -> Self {
        Self {
            attempts: 5,
            base: Duration::from_secs(1),
            cap: Duration::from_secs(32),
            max_retry_after: Duration::from_secs(120),
        }
    }
}

impl RetryPolicy {
    /// Millisecond waits, for tests.
    pub fn quick() -> Self {
        Self {
            attempts: 3,
            base: Duration::from_millis(1),
            cap: Duration::from_millis(5),
            max_retry_after: Duration::from_millis(30),
        }
    }

    /// The wait before retry number `retry` (0 for the first): `Retry-After` when the server
    /// sent one (capped), else `base · 2^retry` capped, plus up to `base` of jitter.
    pub fn delay(&self, retry: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(after) = retry_after {
            return after.min(self.max_retry_after);
        }
        let doubled = self
            .base
            .saturating_mul(1u32 << retry.min(16))
            .min(self.cap);
        let jitter_cap = u64::try_from(self.base.as_millis()).unwrap_or(1_000).max(1);
        doubled + Duration::from_millis(fastrand::u64(0..jitter_cap))
    }
}

/// A token bucket in quota units: `burst` at once, then `per_minute` spread evenly.
pub struct Quota {
    per_second: f64,
    burst: f64,
    state: tokio::sync::Mutex<(f64, Instant)>,
}

impl Quota {
    pub fn new(per_minute: u32, burst: u32) -> Self {
        Self {
            per_second: f64::from(per_minute) / 60.0,
            burst: f64::from(burst),
            state: tokio::sync::Mutex::new((f64::from(burst), Instant::now())),
        }
    }

    /// No pacing, for tests.
    pub fn unlimited() -> Self {
        Self::new(u32::MAX, u32::MAX)
    }

    /// Wait until `units` may be spent. Waiters queue in order behind the lock. A cost above
    /// `burst` waits for a full bucket (it could never fill past it).
    pub async fn take(&self, units: u32) {
        let units = f64::from(units).min(self.burst);
        let mut state = self.state.lock().await;
        loop {
            let (available, since) = *state;
            let now = Instant::now();
            let filled =
                (available + since.elapsed().as_secs_f64() * self.per_second).min(self.burst);
            if filled >= units {
                *state = (filled - units, now);
                return;
            }
            *state = (filled, now);
            let wait = (units - filled) / self.per_second;
            tokio::time::sleep(Duration::from_secs_f64(wait.min(60.0))).await;
        }
    }
}

/// Requests to one account's API.
pub struct ApiClient {
    http: reqwest::Client,
    base: Url,
    tokens: Arc<dyn AccessTokens>,
    policy: RetryPolicy,
    quota: Quota,
    observer: Option<RetryObserver>,
    max_response: usize,
}

/// A JSON answer, or why one that arrived could not be read.
pub(crate) enum Readable<T> {
    Parsed(T),
    Unreadable(ProviderError),
}

/// One try's outcome.
enum Attempt {
    Done(Vec<u8>),
    /// A 2xx answer over the response cap.
    TooLarge,
    Unauthorized(Option<String>),
    Failed(ProviderError),
}

/// The HTTP client for provider APIs: HTTP/2 when the server offers it, no redirects,
/// bounded connects and requests.
pub fn api_http_client() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|e| {
            ProviderError::permanent("could not build the HTTP client")
                .with_detail(e.without_url().to_string())
        })
}

impl ApiClient {
    pub fn new(http: reqwest::Client, base: Url, tokens: Arc<dyn AccessTokens>) -> Self {
        Self {
            http,
            base,
            tokens,
            policy: RetryPolicy::default(),
            quota: Quota::new(QUOTA_PER_MINUTE, QUOTA_BURST),
            observer: None,
            max_response: MAX_RESPONSE_BYTES,
        }
    }

    pub fn with_policy(mut self, policy: RetryPolicy) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_quota(mut self, quota: Quota) -> Self {
        self.quota = quota;
        self
    }

    pub fn with_observer(mut self, observer: RetryObserver) -> Self {
        self.observer = Some(observer);
        self
    }

    pub fn with_max_response(mut self, bytes: usize) -> Self {
        self.max_response = bytes;
        self
    }

    /// GET `path` (relative to the base) with `query`, spending `cost` quota units per try,
    /// and parse the JSON answer.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cost: u32,
    ) -> Result<T, ProviderError> {
        match self.get_readable(path, query, cost).await? {
            Readable::Parsed(value) => Ok(value),
            Readable::Unreadable(error) => Err(error),
        }
    }

    /// As `get_json`, but an answer that arrived and that Rail cannot read (over the response
    /// cap, or not of `T`'s shape) is `Readable::Unreadable` rather than an error, so a caller
    /// can skip that one item and go on.
    pub(crate) async fn get_readable<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cost: u32,
    ) -> Result<Readable<T>, ProviderError> {
        let url = self.resolve(path)?;
        let Some(bytes) = self.get(&url, query, cost).await? else {
            return Ok(Readable::Unreadable(ProviderError::permanent(&format!(
                "Gmail's answer was larger than {} KiB",
                self.max_response / 1024
            ))));
        };
        Ok(match serde_json::from_slice(&bytes) {
            Ok(value) => Readable::Parsed(value),
            Err(e) => Readable::Unreadable(malformed(&e)),
        })
    }

    /// `path` joined to the base, refused unless it stays under the base: paths carry ids
    /// Gmail sent, and a request must never leave the API root (or carry the token elsewhere).
    fn resolve(&self, path: &str) -> Result<Url, ProviderError> {
        let url = self
            .base
            .join(path)
            .map_err(|_| ProviderError::permanent("Rail built an invalid request URL"))?;
        if url.origin() != self.base.origin() || !url.path().starts_with(self.base.path()) {
            return Err(ProviderError::permanent(
                "Rail refused a request URL outside the provider's API",
            ));
        }
        Ok(url)
    }

    async fn get(
        &self,
        url: &Url,
        query: &[(&str, String)],
        cost: u32,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        let mut refreshed = false;
        let mut retry = 0;
        loop {
            self.quota.take(cost).await;
            let token = self.tokens.token().await?;
            match self.attempt(url, query, &token).await {
                Attempt::Done(bytes) => return Ok(Some(bytes)),
                Attempt::TooLarge => return Ok(None),
                Attempt::Unauthorized(_) if !refreshed => {
                    refreshed = true;
                    self.tokens.refresh().await?;
                }
                Attempt::Unauthorized(detail) => return Err(ProviderError::NeedsAuth { detail }),
                Attempt::Failed(ProviderError::Transient { retry_after, .. })
                    if retry + 1 < self.policy.attempts =>
                {
                    let wait = self.policy.delay(retry, retry_after);
                    if let Some(observer) = &self.observer {
                        observer(wait);
                    }
                    tokio::time::sleep(wait).await;
                    retry += 1;
                }
                Attempt::Failed(error) => return Err(error),
            }
        }
    }

    async fn attempt(&self, url: &Url, query: &[(&str, String)], token: &str) -> Attempt {
        let started = Instant::now();
        let sent = self
            .http
            .get(url.clone())
            .query(query)
            .bearer_auth(token)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(e) => return Attempt::Failed(transport(e)),
        };
        let status = response.status();
        tracing::debug!(
            method = "GET",
            path = url.path(),
            status = status.as_u16(),
            elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
            "provider request"
        );
        let retry_after = retry_after(&response);
        let cap = if status.is_success() {
            self.max_response
        } else {
            MAX_ERROR_BODY
        };
        match read_capped(response, cap).await {
            Ok(Some(body)) => classify(status, retry_after, body),
            Ok(None) if status.is_success() => Attempt::TooLarge,
            // An error answer too long to read is judged by its status alone.
            Ok(None) => classify(status, retry_after, Vec::new()),
            Err(error) => Attempt::Failed(error),
        }
    }
}

/// Transport text without the URL, whose query may be the user's search words.
fn transport(error: reqwest::Error) -> ProviderError {
    ProviderError::transient("could not reach Gmail").with_detail(error.without_url().to_string())
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

/// The whole body, or `None` once it passes `cap` (the rest is never read).
async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
) -> Result<Option<Vec<u8>>, ProviderError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport)? {
        if bytes.len() + chunk.len() > cap {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Some(bytes))
}

/// serde's text can quote the answer (mail); keep only its kind and position.
fn malformed(error: &serde_json::Error) -> ProviderError {
    ProviderError::permanent("Gmail's answer did not have the expected shape").with_detail(format!(
        "{:?} at line {} column {}",
        error.classify(),
        error.line(),
        error.column()
    ))
}

#[derive(Deserialize, Default)]
struct GoogleErrorBody {
    #[serde(default)]
    error: GoogleError,
}

#[derive(Deserialize, Default)]
struct GoogleError {
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    errors: Vec<GoogleReason>,
}

#[derive(Deserialize)]
struct GoogleReason {
    #[serde(default)]
    reason: Option<String>,
}

/// What Google said, cut to `MAX_ERROR_TEXT`, and whether it was a rate limit.
fn google_error(body: &[u8]) -> (Option<String>, bool) {
    let parsed: GoogleErrorBody = serde_json::from_slice(body).unwrap_or_default();
    let limited = parsed.error.errors.iter().any(|e| {
        matches!(
            e.reason.as_deref(),
            Some("rateLimitExceeded" | "userRateLimitExceeded")
        )
    });
    let message = parsed
        .error
        .message
        .map(|m| m.chars().take(MAX_ERROR_TEXT).collect());
    (message, limited)
}

fn classify(status: StatusCode, retry_after: Option<Duration>, body: Vec<u8>) -> Attempt {
    if status.is_success() {
        return Attempt::Done(body);
    }
    let (message, rate_limited) = google_error(&body);
    let code = status.as_u16();
    let error = match code {
        401 => return Attempt::Unauthorized(message),
        404 => ProviderError::NotFound,
        429 | 500..=599 => busy(code, retry_after, message),
        403 if rate_limited => busy(code, retry_after, message),
        _ => ProviderError::Permanent {
            message: refused_message(code),
            detail: message,
        },
    };
    Attempt::Failed(error)
}

/// Rail's words for a refusal with this HTTP status.
fn refused_message(code: u16) -> String {
    format!("Gmail refused the request (HTTP {code})")
}

/// Whether `error` is a refusal with HTTP 400 (Bad Request).
pub(crate) fn is_bad_request(error: &ProviderError) -> bool {
    matches!(error, ProviderError::Permanent { message, .. } if *message == refused_message(400))
}

fn busy(code: u16, retry_after: Option<Duration>, detail: Option<String>) -> ProviderError {
    ProviderError::Transient {
        message: format!("Gmail is busy or rate limiting (HTTP {code})"),
        detail,
        retry_after,
    }
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
