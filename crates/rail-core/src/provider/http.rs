//! The HTTP layer providers share: GET and JSON requests with bearer tokens, a quota pacer,
//! retries on 429, 5xx and rate-limit 403s with capped exponential backoff and jitter
//! (honouring `Retry-After` in seconds or as an HTTP-date), one token refresh per request on
//! 401 (shared by every request refused with the same token), a shorter retry policy for
//! interactive work, capped response reads, and two dialects: Google's, and Microsoft Graph's
//! (immutable ids on every request, Graph's error envelope, an optional limiter of requests in
//! flight, `$batch`). An upload session's chunks go without the token (`http_upload.rs`).
//! Requests are logged with method, path, status and duration at debug: never the query
//! string, a body or a token.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use reqwest::StatusCode;
use reqwest::header::RETRY_AFTER;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use url::Url;

use super::ProviderError;
use super::graph::errors::{Refusal, refusal};
use super::tokens::{AccessTokens, RefreshFlight};

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

/// Which API family a client speaks: its name in Rail's messages, how its refusals read, and
/// the preferences every request carries.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Dialect {
    /// Google's REST APIs: `{"error":{"message","errors":[{"reason"}]}}`.
    #[default]
    Google,
    /// Microsoft Graph: `{"error":{"code","message"}}`, and immutable ids on every request.
    Graph,
}

/// Graph's preference for ids that survive a move between folders. It applies only to the
/// request that carries it, so every Graph request carries it.
pub const IMMUTABLE_IDS: &str = "IdType=\"ImmutableId\"";

impl Dialect {
    /// The provider's name in Rail's own messages.
    pub fn name(self) -> &'static str {
        match self {
            Self::Google => "Gmail",
            Self::Graph => "Microsoft 365",
        }
    }

    /// The `Prefer` header of a request asking for `extra`, if it has one.
    fn prefer(self, extra: &[&str]) -> Option<String> {
        let mut all: Vec<&str> = Vec::new();
        if self == Self::Graph {
            all.push(IMMUTABLE_IDS);
        }
        all.extend_from_slice(extra);
        (!all.is_empty()).then(|| all.join(", "))
    }
}

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

    /// For a person waiting on the answer (`rail search`): one retry, at most 2 s away.
    pub fn interactive() -> Self {
        Self {
            attempts: 2,
            base: Duration::from_millis(500),
            cap: Duration::from_secs(1),
            max_retry_after: Duration::from_secs(2),
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

tokio::task_local! {
    /// Set by `with_retry_policy`: the policy that replaces each client's own.
    static SCOPED_POLICY: RetryPolicy;
}

/// Run `work` with every request inside it retried on `policy` instead of its client's own.
/// The engine shares its client with interactive work, so the policy follows the work, not
/// the client.
pub(crate) async fn with_retry_policy<F: std::future::Future>(
    policy: RetryPolicy,
    work: F,
) -> F::Output {
    SCOPED_POLICY.scope(policy, work).await
}

/// The policy `with_retry_policy` set for the current work, if any.
pub(crate) fn retry_policy_override() -> Option<RetryPolicy> {
    SCOPED_POLICY.try_with(|policy| *policy).ok()
}

/// A JSON body as bytes.
fn encode(body: Option<&serde_json::Value>) -> Result<Option<Vec<u8>>, ProviderError> {
    body.map(serde_json::to_vec)
        .transpose()
        .map_err(|_| ProviderError::permanent("Rail could not encode a request"))
}

/// A token bucket in quota units: `burst` at once, then `per_minute` spread evenly.
pub struct Quota {
    per_second: f64,
    burst: f64,
    state: tokio::sync::Mutex<(f64, Instant)>,
    /// Units asked for, for tests that check a call's cost.
    #[cfg(test)]
    taken: std::sync::atomic::AtomicU64,
}

impl Quota {
    pub fn new(per_minute: u32, burst: u32) -> Self {
        Self {
            per_second: f64::from(per_minute) / 60.0,
            burst: f64::from(burst),
            state: tokio::sync::Mutex::new((f64::from(burst), Instant::now())),
            #[cfg(test)]
            taken: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// No pacing, for tests.
    pub fn unlimited() -> Self {
        Self::new(u32::MAX, u32::MAX)
    }

    /// Wait until `units` may be spent. Waiters queue in order behind the lock. A cost above
    /// `burst` waits for a full bucket (it could never fill past it).
    pub async fn take(&self, units: u32) {
        #[cfg(test)]
        self.taken
            .fetch_add(u64::from(units), std::sync::atomic::Ordering::SeqCst);
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
    tokens: RefreshFlight,
    policy: RetryPolicy,
    quota: Quota,
    observer: Option<RetryObserver>,
    max_response: usize,
    dialect: Dialect,
    /// Requests in flight at once, when limited (`with_concurrency`).
    limiter: Option<Arc<tokio::sync::Semaphore>>,
    permits: u32,
}

/// One request the loop sends: GET with a query, or another method with an optional JSON body;
/// the preferences it adds to its dialect's, and the permits it takes (`u32::MAX`: all).
struct Outgoing<'a> {
    method: reqwest::Method,
    query: &'a [(&'a str, String)],
    body: Option<Vec<u8>>,
    prefer: &'a [&'a str],
    weight: u32,
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
            tokens: RefreshFlight::new(tokens),
            policy: RetryPolicy::default(),
            quota: Quota::new(QUOTA_PER_MINUTE, QUOTA_BURST),
            observer: None,
            max_response: MAX_RESPONSE_BYTES,
            dialect: Dialect::Google,
            limiter: None,
            permits: 0,
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

    pub fn with_dialect(mut self, dialect: Dialect) -> Self {
        self.dialect = dialect;
        self
    }

    /// At most `permits` requests in flight at once: each try takes one until its answer is
    /// read, and a `$batch` takes them all.
    pub fn with_concurrency(mut self, permits: u32) -> Self {
        let permits = permits.max(1);
        self.limiter = Some(Arc::new(tokio::sync::Semaphore::new(permits as usize)));
        self.permits = permits;
        self
    }

    /// GET `path` (relative to the base, or an absolute link under it) with `query`, spending
    /// `cost` quota units per try, and parse the JSON answer.
    pub async fn get_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cost: u32,
    ) -> Result<T, ProviderError> {
        self.get_json_with(path, query, cost, &[]).await
    }

    /// `get_json` with `prefer` added to the dialect's `Prefer` header.
    pub async fn get_json_with<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cost: u32,
        prefer: &[&str],
    ) -> Result<T, ProviderError> {
        match self.get_readable_with(path, query, cost, prefer).await? {
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
        self.get_readable_with(path, query, cost, &[]).await
    }

    async fn get_readable_with<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cost: u32,
        prefer: &[&str],
    ) -> Result<Readable<T>, ProviderError> {
        let url = self.resolve(path)?;
        let outgoing = Outgoing {
            method: reqwest::Method::GET,
            query,
            body: None,
            prefer,
            weight: 1,
        };
        let name = self.dialect.name();
        let Some(bytes) = self.send(&url, &outgoing, cost).await? else {
            return Ok(Readable::Unreadable(ProviderError::permanent(&format!(
                "{name}'s answer was larger than {} KiB",
                self.max_response / 1024
            ))));
        };
        Ok(match serde_json::from_slice(&bytes) {
            Ok(value) => Readable::Parsed(value),
            Err(e) => Readable::Unreadable(malformed(name, &e)),
        })
    }

    /// POST `path` with `body` as JSON (an empty body when `None`), spending `cost` per try,
    /// through the same pacing, retries, 401 refresh and origin check as a GET. The answer is
    /// not read beyond its status: a 2xx (even one over the response cap) is success.
    pub(crate) async fn post_json(
        &self,
        path: &str,
        body: Option<&serde_json::Value>,
        cost: u32,
    ) -> Result<(), ProviderError> {
        let url = self.resolve(path)?;
        let body = body
            .map(serde_json::to_vec)
            .transpose()
            .map_err(|_| ProviderError::permanent("Rail could not encode a request"))?;
        let outgoing = Outgoing {
            method: reqwest::Method::POST,
            query: &[],
            body: Some(body.unwrap_or_default()),
            prefer: &[],
            weight: 1,
        };
        self.send(&url, &outgoing, cost).await.map(|_| ())
    }

    /// `post_json`, the answer returned unparsed: `Ok` is a 2xx answer, its body `None` when it
    /// was too large to read (a send's success matters even when its answer is unreadable).
    /// With `retries` false the request is tried once (plus the one 401 refresh): a send that
    /// may have reached Gmail must not be repeated blindly; the outbox worker checks first.
    pub(crate) async fn post_json_bytes(
        &self,
        path: &str,
        body: Option<&serde_json::Value>,
        cost: u32,
        retries: bool,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        let encoded = encode(body)?.unwrap_or_default();
        self.request_encoded(reqwest::Method::POST, path, Some(encoded), cost, retries)
            .await
    }

    /// One request with `method` and an optional JSON `body` (none is sent when `None`), the
    /// answer unparsed like `post_json_bytes`; `retries` means the same.
    pub(crate) async fn request_bytes(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
        cost: u32,
        retries: bool,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        let encoded = encode(body)?;
        self.request_encoded(method, path, encoded, cost, retries)
            .await
    }

    async fn request_encoded(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Vec<u8>>,
        cost: u32,
        retries: bool,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        let url = self.resolve(path)?;
        let outgoing = Outgoing {
            method,
            query: &[],
            body,
            prefer: &[],
            weight: 1,
        };
        if retries {
            return self.send(&url, &outgoing, cost).await;
        }
        let policy = RetryPolicy {
            attempts: 1,
            ..retry_policy_override().unwrap_or(self.policy)
        };
        with_retry_policy(policy, self.send(&url, &outgoing, cost)).await
    }

    /// POST Graph's `$batch` with `body`, spending `cost` per try, and holding every permit:
    /// Graph runs up to four of the batch's requests at once against the mailbox, the whole of
    /// Outlook's concurrency limit. The answer is returned unparsed (`None` over the cap).
    pub async fn post_batch(
        &self,
        body: &serde_json::Value,
        cost: u32,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        let url = self.resolve("$batch")?;
        let outgoing = Outgoing {
            method: reqwest::Method::POST,
            query: &[],
            body: encode(Some(body))?,
            prefer: &[],
            weight: u32::MAX,
        };
        self.send(&url, &outgoing, cost).await
    }

    /// Quota units taken so far, retries included.
    #[cfg(test)]
    pub(crate) fn units_taken(&self) -> u64 {
        self.quota.taken.load(std::sync::atomic::Ordering::SeqCst)
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

    async fn send(
        &self,
        url: &Url,
        outgoing: &Outgoing<'_>,
        cost: u32,
    ) -> Result<Option<Vec<u8>>, ProviderError> {
        let policy = retry_policy_override().unwrap_or(self.policy);
        // The token this request's one refresh returned: the retry uses it, not a re-read.
        let mut renewed: Option<String> = None;
        let mut refreshed = false;
        let mut reused_once = false;
        let mut retry = 0;
        loop {
            self.quota.take(cost).await;
            let token = match renewed.take() {
                Some(token) => token,
                None => self.tokens.token().await?,
            };
            match self.attempt(url, outgoing, &token).await {
                Attempt::Done(bytes) => return Ok(Some(bytes)),
                Attempt::TooLarge => return Ok(None),
                Attempt::Unauthorized(_) if !refreshed => {
                    let (fresh, reused) = self.tokens.refresh_after(&token, !reused_once).await?;
                    // A reused result is not a refresh made for this request: if its token is
                    // refused too, the request gets its one real refresh. Reuse happens at most
                    // once, so a request makes at most three authorised attempts.
                    // Any refresh after a reuse is the request's real one, however it was
                    // reported: the cap holds on this side too.
                    refreshed = !reused || reused_once;
                    reused_once |= reused;
                    renewed = Some(fresh);
                }
                Attempt::Unauthorized(detail) => return Err(ProviderError::NeedsAuth { detail }),
                Attempt::Failed(ProviderError::Transient { retry_after, .. })
                    if retry + 1 < policy.attempts =>
                {
                    let wait = policy.delay(retry, retry_after);
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

    async fn attempt(&self, url: &Url, outgoing: &Outgoing<'_>, token: &str) -> Attempt {
        // Held until the answer is read: the limiter counts requests in flight.
        let _permit = match self.permit(outgoing.weight).await {
            Ok(permit) => permit,
            Err(error) => return Attempt::Failed(error),
        };
        let name = self.dialect.name();
        let started = Instant::now();
        let response = match self.request(url, outgoing, token).send().await {
            Ok(response) => response,
            Err(e) => return Attempt::Failed(transport(name, e)),
        };
        let status = response.status();
        tracing::debug!(
            method = outgoing.method.as_str(),
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
        match read_capped(response, cap, name).await {
            Ok(Some(body)) => classify(self.dialect, status, retry_after, body),
            Ok(None) if status.is_success() => Attempt::TooLarge,
            // An error answer too long to read is judged by its status alone.
            Ok(None) => classify(self.dialect, status, retry_after, Vec::new()),
            Err(error) => Attempt::Failed(error),
        }
    }

    fn request(&self, url: &Url, outgoing: &Outgoing<'_>, token: &str) -> reqwest::RequestBuilder {
        let mut request = self
            .http
            .request(outgoing.method.clone(), url.clone())
            .query(outgoing.query)
            .bearer_auth(token);
        if let Some(prefer) = self.dialect.prefer(outgoing.prefer) {
            request = request.header("Prefer", prefer);
        }
        if let Some(body) = &outgoing.body {
            request = request
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.clone());
            // hyper sends no length for an empty body, and Graph answers 411 to a bodiless POST.
            if body.is_empty() {
                request = request.header(reqwest::header::CONTENT_LENGTH, "0");
            }
        }
        request
    }

    /// `weight` permits of the limiter (all of them at most), or none when unlimited.
    async fn permit(
        &self,
        weight: u32,
    ) -> Result<Option<tokio::sync::OwnedSemaphorePermit>, ProviderError> {
        let Some(limiter) = &self.limiter else {
            return Ok(None);
        };
        limiter
            .clone()
            .acquire_many_owned(weight.clamp(1, self.permits))
            .await
            .map(Some)
            .map_err(|_| ProviderError::transient("the request limiter closed"))
    }
}

/// Transport text without the URL, whose query may be the user's search words.
fn transport(name: &str, error: reqwest::Error) -> ProviderError {
    ProviderError::transient(&format!("could not reach {name}"))
        .with_detail(error.without_url().to_string())
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    let value = response.headers().get(RETRY_AFTER)?.to_str().ok()?;
    parse_retry_after(value, SystemTime::now())
}

/// `Retry-After` as a wait from `now` (RFC 9110 §10.2.3): delay-seconds (ASCII digits only; a
/// number past `u64` saturates), or an HTTP-date in any of its three forms (IMF-fixdate,
/// RFC 850, asctime). A date already past is no wait. Anything else is `None`, so the backoff
/// applies. `RetryPolicy::delay` caps the result.
pub(crate) fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    if !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()) {
        return Some(
            value
                .parse::<u64>()
                .map_or(Duration::MAX, Duration::from_secs),
        );
    }
    let at = httpdate::parse_http_date(value).ok()?;
    Some(at.duration_since(now).unwrap_or(Duration::ZERO))
}

/// The whole body, or `None` once it passes `cap` (the rest is never read).
async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
    name: &str,
) -> Result<Option<Vec<u8>>, ProviderError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| transport(name, e))? {
        if bytes.len() + chunk.len() > cap {
            return Ok(None);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(Some(bytes))
}

/// serde's text can quote the answer (mail); keep only its kind and position.
fn malformed(name: &str, error: &serde_json::Error) -> ProviderError {
    ProviderError::permanent(&format!("{name}'s answer did not have the expected shape"))
        .with_detail(format!(
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

fn classify(
    dialect: Dialect,
    status: StatusCode,
    retry_after: Option<Duration>,
    body: Vec<u8>,
) -> Attempt {
    if status.is_success() {
        return Attempt::Done(body);
    }
    match dialect {
        Dialect::Google => google_refusal(status, retry_after, &body),
        Dialect::Graph => match refusal(status.as_u16(), retry_after, &body) {
            Refusal::Unauthorized(detail) => Attempt::Unauthorized(detail),
            Refusal::Failed(error) => Attempt::Failed(error),
        },
    }
}

fn google_refusal(status: StatusCode, retry_after: Option<Duration>, body: &[u8]) -> Attempt {
    let (message, rate_limited) = google_error(body);
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

#[path = "http_upload.rs"]
mod upload;
pub(crate) use upload::ChunkAnswer;

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
