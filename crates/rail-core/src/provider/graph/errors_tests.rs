//! Graph's refusals: each row of the classification, read from the status and the envelope,
//! and through the client (one refresh on 401, `Retry-After` on 429, Rail's words for
//! transport trouble and an answer of the wrong shape).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use url::Url;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::errors::{CONSENT_MESSAGE, MAILBOX_MESSAGE, Refusal, on_delta, refusal};
use crate::provider::ProviderError;
use crate::provider::http::{ApiClient, Dialect, Quota, RetryPolicy, api_http_client};
use crate::provider::tokens::FixedTokens;

fn envelope(code: &str, message: &str) -> Vec<u8> {
    json!({"error": {"code": code, "message": message, "innerError": {"request-id": "r1"}}})
        .to_string()
        .into_bytes()
}

fn failed(status: u16, code: &str) -> ProviderError {
    match refusal(status, None, &envelope(code, "said Graph")) {
        Refusal::Failed(error) => error,
        Refusal::Unauthorized(_) => panic!("{status} {code} read as unauthorized"),
    }
}

#[test]
fn each_status_and_code_reads_as_the_spec_says() {
    assert!(matches!(
        refusal(401, None, &envelope("InvalidAuthenticationToken", "expired")),
        Refusal::Unauthorized(Some(d)) if d == "InvalidAuthenticationToken: expired"
    ));
    assert_eq!(
        failed(403, "ErrorAccessDenied"),
        ProviderError::Permanent {
            message: CONSENT_MESSAGE.into(),
            detail: Some("ErrorAccessDenied: said Graph".into()),
        }
    );
    assert_eq!(
        failed(403, "Authorization_RequestDenied").to_string(),
        CONSENT_MESSAGE
    );
    assert_eq!(
        failed(404, "MailboxNotEnabledForRESTAPI").to_string(),
        MAILBOX_MESSAGE
    );
    assert_eq!(
        failed(401, "mailboxnotenabledforrestapi").to_string(),
        MAILBOX_MESSAGE
    );
    assert_eq!(failed(404, "ErrorItemNotFound"), ProviderError::NotFound);
    assert_eq!(
        failed(410, "Gone"),
        ProviderError::Permanent {
            message: "Microsoft 365 refused the request (HTTP 410)".into(),
            detail: Some("Gone: said Graph".into()),
        },
        "a 410 outside a delta is a refusal (#31)"
    );
    for (status, code) in [(400, "syncStateNotFound"), (409, "ResyncRequired")] {
        assert!(
            matches!(failed(status, code), ProviderError::Permanent { .. }),
            "{status} {code}: a refusal outside a delta"
        );
    }
    for status in [429, 500, 502, 503, 504] {
        assert!(
            matches!(failed(status, "x"), ProviderError::Transient { .. }),
            "{status}"
        );
    }
    assert_eq!(
        failed(400, "ErrorInvalidRequest"),
        ProviderError::Permanent {
            message: "Microsoft 365 refused the request (HTTP 400)".into(),
            detail: Some("ErrorInvalidRequest: said Graph".into()),
        }
    );
    assert_eq!(
        failed(403, "ErrorQuotaExceeded").to_string(),
        "Microsoft 365 refused the request (HTTP 403)"
    );
}

#[test]
fn a_429_keeps_its_retry_after_and_graph_text_is_only_ever_detail() {
    let answer = refusal(
        429,
        Some(Duration::from_secs(10)),
        &envelope("TooManyRequests", "<script>x</script>"),
    );
    let Refusal::Failed(ProviderError::Transient {
        message,
        detail,
        retry_after,
    }) = answer
    else {
        panic!("a 429 is transient");
    };
    assert_eq!(message, "Microsoft 365 is busy or unavailable (HTTP 429)");
    assert_eq!(
        detail.as_deref(),
        Some("TooManyRequests: <script>x</script>")
    );
    assert_eq!(retry_after, Some(Duration::from_secs(10)));
}

#[test]
fn an_unreadable_or_huge_envelope_is_judged_by_its_status_and_cut() {
    let Refusal::Failed(gone) = refusal(404, None, b"<html>not json</html>") else {
        panic!("a 404 is not a login failure");
    };
    assert_eq!(gone, ProviderError::NotFound);
    let long = "x".repeat(2_000);
    let Refusal::Failed(error) = refusal(400, None, &envelope(&long, &long)) else {
        panic!("a 400 is not a login failure");
    };
    assert!(error.detail().unwrap().chars().count() == 1_002, "cut");
}

fn client(base: &str, tokens: Arc<FixedTokens>) -> (ApiClient, Arc<Mutex<Vec<Duration>>>) {
    let waits = Arc::new(Mutex::new(Vec::new()));
    let seen = waits.clone();
    let api = ApiClient::new(
        api_http_client().unwrap(),
        Url::parse(base).unwrap(),
        tokens,
    )
    .with_policy(RetryPolicy::quick())
    .with_quota(Quota::unlimited())
    .with_dialect(Dialect::Graph)
    .with_observer(Arc::new(move |wait| seen.lock().unwrap().push(wait)));
    (api, waits)
}

#[tokio::test]
async fn a_401_refreshes_once_then_needs_a_login() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(401).set_body_json(
            json!({"error": {"code": "InvalidAuthenticationToken", "message": "expired"}}),
        ))
        .expect(2)
        .mount(&server)
        .await;
    let tokens = FixedTokens::new(["first", "second"]);
    let (api, _) = client(&format!("{}/v1.0/", server.uri()), tokens.clone());
    let error = api
        .get_json::<serde_json::Value>("me", &[], 1)
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::NeedsAuth { .. }),
        "{error:?}"
    );
    assert_eq!(tokens.refreshes(), 1);
}

#[tokio::test]
async fn a_429_waits_as_long_as_retry_after_says_within_the_cap() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(429).insert_header("Retry-After", "7"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"ok": true})))
        .mount(&server)
        .await;
    let (api, waits) = client(&format!("{}/v1.0/", server.uri()), FixedTokens::new(["t"]));
    api.get_json::<serde_json::Value>("me", &[], 1)
        .await
        .unwrap();
    assert_eq!(*waits.lock().unwrap(), [Duration::from_millis(30)]);
}

#[tokio::test]
async fn transport_trouble_and_a_wrong_shape_name_microsoft_365() {
    let (api, _) = client("http://127.0.0.1:9/v1.0/", FixedTokens::new(["t"]));
    let error = api
        .get_json::<serde_json::Value>("me", &[], 1)
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), "could not reach Microsoft 365");
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_string("not json"))
        .mount(&server)
        .await;
    let (api, _) = client(&format!("{}/v1.0/", server.uri()), FixedTokens::new(["t"]));
    let error = api
        .get_json::<serde_json::Value>("me", &[], 1)
        .await
        .unwrap_err();
    assert_eq!(
        error.to_string(),
        "Microsoft 365's answer did not have the expected shape"
    );
}

#[test]
fn only_a_delta_reads_a_410_as_an_expired_cursor() {
    assert_eq!(on_delta(failed(410, "Gone")), ProviderError::CursorExpired);
    for (status, code) in [
        (400, "syncStateNotFound"),
        (409, "ResyncRequired"),
        (404, "syncStateNotFound"),
    ] {
        assert_eq!(on_delta(failed(status, code)), ProviderError::CursorExpired);
    }
    assert_eq!(
        on_delta(failed(404, "ErrorItemNotFound")),
        ProviderError::NotFound
    );
    let refused = failed(400, "ErrorInvalidRequest");
    assert_eq!(on_delta(refused.clone()), refused);
}
