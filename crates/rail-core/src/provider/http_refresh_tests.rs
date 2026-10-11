//! The 401 refresh: one shared flight per account, reuse of a finished refresh, and the
//! bounds on a request's authorised attempts.

use super::*;
use crate::error::ErrorClass;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Semaphore;

/// Longest any concurrent test may take: a deadlock fails instead of hanging.
const BOUND: Duration = Duration::from_secs(20);

/// Tokens for one account whose refresh counts its calls and, when gated, completes only once
/// the test opens the gate. `old` until a refresh succeeds, `new` after; with `fail`, every
/// refresh fails (transient).
struct SlowTokens {
    old: &'static str,
    new: &'static str,
    fail: bool,
    refreshes: AtomicUsize,
    renewed: AtomicBool,
    gate: Semaphore,
}

impl SlowTokens {
    /// A refresh that waits for `open`.
    fn gated(old: &'static str, new: &'static str, fail: bool) -> Arc<Self> {
        Arc::new(Self {
            old,
            new,
            fail,
            refreshes: AtomicUsize::new(0),
            renewed: AtomicBool::new(false),
            gate: Semaphore::new(0),
        })
    }

    fn open(&self) {
        self.gate.add_permits(Semaphore::MAX_PERMITS / 2);
    }

    fn refreshes(&self) -> usize {
        self.refreshes.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl AccessTokens for SlowTokens {
    async fn token(&self) -> Result<String, ProviderError> {
        let renewed = self.renewed.load(Ordering::SeqCst);
        Ok(if renewed { self.new } else { self.old }.to_string())
    }

    async fn refresh(&self) -> Result<String, ProviderError> {
        self.refreshes.fetch_add(1, Ordering::SeqCst);
        self.gate.acquire().await.unwrap().forget();
        if self.fail {
            return Err(ProviderError::transient(
                "could not reach the token endpoint",
            ));
        }
        self.renewed.store(true, Ordering::SeqCst);
        Ok(self.new.to_string())
    }
}

/// Open `tokens`' gate once `burst` requests of `api` have joined the refresh flight.
async fn open_when_joined(api: &ApiClient, burst: usize, tokens: &SlowTokens) {
    while api.tokens.arrivals.load(Ordering::SeqCst) < burst {
        tokio::task::yield_now().await;
    }
    tokens.open();
}

/// A client for the account whose API lives under `/{root}/gmail/v1/users/me/`.
fn client_under(server: &MockServer, root: &str, tokens: Arc<dyn AccessTokens>) -> ApiClient {
    let base = Url::parse(&format!("{}/{root}/gmail/v1/users/me/", server.uri())).unwrap();
    ApiClient::new(api_http_client().unwrap(), base, tokens)
        .with_policy(RetryPolicy::quick())
        .with_quota(Quota::unlimited())
}

/// The account's API refuses `old` and accepts `new`.
async fn refuse_then_accept(server: &MockServer, root: &str, old: &str, new: &str) {
    Mock::given(path(format!("/{root}/gmail/v1/users/me/labels")))
        .and(bearer_token(old))
        .respond_with(google_error(401, "authError", "Invalid Credentials"))
        .mount(server)
        .await;
    Mock::given(path(format!("/{root}/gmail/v1/users/me/labels")))
        .and(bearer_token(new))
        .respond_with(ok())
        .mount(server)
        .await;
}

/// Two requests refused with the same token share one refresh and retry with its token.
#[tokio::test]
async fn concurrent_401s_share_one_refresh() {
    tokio::time::timeout(BOUND, async {
        let server = MockServer::start().await;
        refuse_then_accept(&server, "a", "old", "new").await;
        let tokens = SlowTokens::gated("old", "new", false);
        let api = client_under(&server, "a", tokens.clone());
        let (one, two, ()) = tokio::join!(get(&api), get(&api), open_when_joined(&api, 2, &tokens));
        assert!(one.unwrap().ok && two.unwrap().ok);
        assert_eq!(tokens.refreshes(), 1, "one refresh for the burst");
        let sent = server.received_requests().await.unwrap();
        assert_eq!(sent.len(), 4, "two refused, two retried");
    })
    .await
    .expect("deadlocked");
}

/// A failed refresh fails every request that waited for it, with the same error; a later
/// refusal tries a new refresh instead of reusing the failure.
#[tokio::test]
async fn a_failed_shared_refresh_fails_every_waiter_alike() {
    tokio::time::timeout(BOUND, async {
        let server = MockServer::start().await;
        refuse_then_accept(&server, "a", "old", "new").await;
        let tokens = SlowTokens::gated("old", "new", true);
        let api = client_under(&server, "a", tokens.clone());
        let (one, two, ()) = tokio::join!(get(&api), get(&api), open_when_joined(&api, 2, &tokens));
        let (one, two) = (one.unwrap_err(), two.unwrap_err());
        assert_eq!(one, two);
        assert_eq!(one.class(), ErrorClass::Transient, "{one:?}");
        assert_eq!(tokens.refreshes(), 1);
        assert!(get(&api).await.is_err());
        assert_eq!(
            tokens.refreshes(),
            2,
            "a failure is not remembered past its burst"
        );
    })
    .await
    .expect("deadlocked");
}

/// Review Focus 4: a 401 storm across two accounts. One refresh per account; account A's
/// failing refresh does not fail account B's requests; no request carries the other
/// account's token.
#[tokio::test]
async fn a_401_storm_across_two_accounts_refreshes_once_per_account() {
    tokio::time::timeout(BOUND, async {
        let server = MockServer::start().await;
        refuse_then_accept(&server, "a", "a-old", "a-new").await;
        refuse_then_accept(&server, "b", "b-old", "b-new").await;
        let ta = SlowTokens::gated("a-old", "a-new", true);
        let tb = SlowTokens::gated("b-old", "b-new", false);
        let a = client_under(&server, "a", ta.clone());
        let b = client_under(&server, "b", tb.clone());
        let (a1, a2, b1, b2, (), ()) = tokio::join!(
            get(&a),
            get(&a),
            get(&b),
            get(&b),
            open_when_joined(&a, 2, &ta),
            open_when_joined(&b, 2, &tb)
        );
        let (a1, a2) = (a1.unwrap_err(), a2.unwrap_err());
        assert_eq!(a1, a2);
        assert!(b1.unwrap().ok && b2.unwrap().ok);
        assert_eq!((ta.refreshes(), tb.refreshes()), (1, 1));
        for request in server.received_requests().await.unwrap() {
            let root = &request.url.path()[1..2];
            let auth = request
                .headers
                .get("authorization")
                .unwrap()
                .to_str()
                .unwrap();
            assert!(
                auth.starts_with(&format!("Bearer {root}-")),
                "{} {auth}",
                request.url.path()
            );
        }
    })
    .await
    .expect("deadlocked");
}

/// Tokens whose cache is never updated: `token()` keeps giving `old` and each refresh gives
/// the next of `fresh`, as a request that read its token before an earlier refresh would see.
struct StaleTokens {
    fresh: Vec<&'static str>,
    refreshes: AtomicUsize,
}

#[async_trait::async_trait]
impl AccessTokens for StaleTokens {
    async fn token(&self) -> Result<String, ProviderError> {
        Ok("old".to_string())
    }

    async fn refresh(&self) -> Result<String, ProviderError> {
        let n = self.refreshes.fetch_add(1, Ordering::SeqCst);
        Ok(self.fresh[n].to_string())
    }
}

/// A request still holding the old token after a successful refresh reuses its result (no
/// second refresh). That reuse is not the request's own refresh: if the reused token is
/// refused too, the request starts a real refresh and succeeds with its token, not `NeedsAuth`.
#[tokio::test]
async fn a_reused_refresh_does_not_use_up_the_requests_own() {
    tokio::time::timeout(BOUND, async {
        let server = MockServer::start().await;
        refuse_then_accept(&server, "a", "old", "new1").await;
        let tokens = Arc::new(StaleTokens {
            fresh: vec!["new1", "new2"],
            refreshes: AtomicUsize::new(0),
        });
        let api = client_under(&server, "a", tokens.clone());
        assert!(get(&api).await.unwrap().ok);
        assert_eq!(tokens.refreshes.load(Ordering::SeqCst), 1);

        // (a) The API now refuses `new1` as well; `old` is still what this request holds.
        server.reset().await;
        refuse_then_accept(&server, "a", "old", "new2").await;
        Mock::given(path("/a/gmail/v1/users/me/labels"))
            .and(bearer_token("new1"))
            .respond_with(google_error(401, "authError", "Invalid Credentials"))
            .mount(&server)
            .await;
        // The request reuses the finished refresh (`new1`, no refresh), is refused again, and
        // (b) starts one real refresh (`new2`).
        assert!(get(&api).await.unwrap().ok);
        assert_eq!(
            tokens.refreshes.load(Ordering::SeqCst),
            2,
            "one reuse, one real"
        );
        let sent = server.received_requests().await.unwrap();
        let bearers: Vec<_> = sent
            .iter()
            .map(|r| {
                r.headers
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_string()
            })
            .collect();
        assert_eq!(bearers, ["Bearer old", "Bearer new1", "Bearer new2"]);
    })
    .await
    .expect("deadlocked");
}

/// `StaleTokens` against an API that refuses everything: one request does the setup (a real
/// refresh whose result stays cached), a second one is measured. Returns the refusal, the
/// refreshes in all, and the authorised attempts the second request made.
async fn refused_twice_over(fresh: Vec<&'static str>) -> (ProviderError, usize, usize) {
    let server = MockServer::start().await;
    Mock::given(path("/a/gmail/v1/users/me/labels"))
        .respond_with(google_error(401, "authError", "Invalid Credentials"))
        .mount(&server)
        .await;
    let tokens = Arc::new(StaleTokens {
        fresh,
        refreshes: AtomicUsize::new(0),
    });
    let api = client_under(&server, "a", tokens.clone());
    assert!(get(&api).await.is_err());
    let before = server.received_requests().await.unwrap().len();
    let err = get(&api).await.unwrap_err();
    let attempts = server.received_requests().await.unwrap().len() - before;
    (err, tokens.refreshes.load(Ordering::SeqCst), attempts)
}

/// Reused result refused, then this request's own real refresh refused: `NeedsAuth` after
/// exactly one real refresh by this request and three authorised attempts.
#[tokio::test]
async fn a_reused_then_a_real_refresh_both_refused_is_needs_auth() {
    tokio::time::timeout(BOUND, async {
        let (err, refreshes, attempts) = refused_twice_over(vec!["new1", "new2"]).await;
        assert!(matches!(err, ProviderError::NeedsAuth { .. }), "{err:?}");
        assert_eq!(refreshes, 2, "the setup's refresh and this request's one");
        assert_eq!(attempts, 3, "original, one reused, one refreshed");
    })
    .await
    .expect("deadlocked");
}

/// The chain case: the finished refresh keeps answering with the refused token itself, so
/// every reuse would be refused again. A request reuses at most once, then refreshes for real.
#[tokio::test]
async fn a_request_reuses_a_finished_refresh_at_most_once() {
    tokio::time::timeout(BOUND, async {
        let (err, refreshes, attempts) = refused_twice_over(vec!["old", "old", "old"]).await;
        assert!(matches!(err, ProviderError::NeedsAuth { .. }), "{err:?}");
        assert_eq!(refreshes, 2, "one reuse, then one real refresh");
        assert_eq!(attempts, 3, "never more than three authorised attempts");
    })
    .await
    .expect("deadlocked");
}
