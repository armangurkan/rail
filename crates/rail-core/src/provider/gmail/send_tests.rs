//! `messages.send` and the `rfc822msgid` lookup against fake Gmail.

use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{API_PATH, Gmail};
use crate::provider::{ProviderError, SentReceipt};

#[tokio::test]
async fn send_posts_raw_base64url_with_the_thread_id_and_returns_the_receipt() {
    let gmail = Gmail::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/messages/send")))
        .and(body_json(json!({"raw": "SGVsbG8", "threadId": "t1"})))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "m9", "threadId": "t1"})),
        )
        .expect(1)
        .mount(&gmail.server)
        .await;
    let receipt = gmail
        .provider()
        .send_raw(b"Hello", Some("t1"))
        .await
        .unwrap();
    assert_eq!(
        receipt,
        SentReceipt {
            message_id: Some("m9".into()),
            thread_id: Some("t1".into())
        }
    );
}

/// Fix round 1: a 2xx Rail cannot read is still a send, never an error the user would retry.
#[tokio::test]
async fn a_2xx_with_an_unreadable_answer_is_a_send_with_unknown_ids() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    let unknown = SentReceipt {
        message_id: None,
        thread_id: None,
    };
    for answer in [
        ResponseTemplate::new(200).set_body_json(json!({"nope": 1})),
        ResponseTemplate::new(200).set_body_json(json!({"id": "bad id", "threadId": "t1"})),
        ResponseTemplate::new(200).set_body_string("not json"),
    ] {
        gmail.server.reset().await;
        Mock::given(method("POST"))
            .and(path(format!("{API_PATH}/messages/send")))
            .respond_with(answer)
            .expect(1)
            .mount(&gmail.server)
            .await;
        assert_eq!(provider.send_raw(b"Hello", None).await.unwrap(), unknown);
    }
}

#[tokio::test]
async fn send_without_a_thread_omits_the_field_and_a_400_is_permanent_and_a_429_transient() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/messages/send")))
        .and(body_json(json!({"raw": "SGVsbG8"})))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_json(json!({"error": {"message": "Invalid To header"}})),
        )
        .expect(1)
        .mount(&gmail.server)
        .await;
    let error = provider.send_raw(b"Hello", None).await.unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "{error:?}"
    );
    assert_eq!(error.detail(), Some("Invalid To header"));
    gmail.server.reset().await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/messages/send")))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let error = provider.send_raw(b"Hello", None).await.unwrap_err();
    assert!(
        matches!(error, ProviderError::Transient { .. }),
        "a send is never retried inside the HTTP layer: one request"
    );
}

#[tokio::test]
async fn send_refuses_a_thread_id_that_is_not_a_plain_id() {
    let gmail = Gmail::start().await;
    let error = gmail
        .provider()
        .send_raw(b"Hello", Some("t1/../x"))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn message_by_header_id_queries_rfc822msgid_and_maps_an_empty_list_to_none() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages")))
        .and(query_param("q", "rfc822msgid:<abc@example.com>"))
        .and(query_param("maxResults", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"messages": [{"id": "m1", "threadId": "t1"}]})),
        )
        .expect(1)
        .mount(&gmail.server)
        .await;
    assert_eq!(
        provider
            .find_by_header_id("<abc@example.com>")
            .await
            .unwrap(),
        Some("m1".into())
    );
    gmail.server.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"resultSizeEstimate": 0})))
        .mount(&gmail.server)
        .await;
    assert_eq!(
        provider
            .find_by_header_id("<abc@example.com>")
            .await
            .unwrap(),
        None
    );
    gmail.server.reset().await;
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"messages": [{"id": "bad id"}]})),
        )
        .mount(&gmail.server)
        .await;
    let error = provider
        .find_by_header_id("<abc@example.com>")
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "a found message with an unusable id is an error, not None: {error:?}"
    );
    let error = provider.find_by_header_id("abc\"def").await.unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "a header id with a quote is refused"
    );
}
