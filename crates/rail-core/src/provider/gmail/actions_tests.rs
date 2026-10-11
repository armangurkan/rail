//! `threads.modify` and `threads.trash` against fake Gmail: exact bodies, one call per thread,
//! the error classes, refused ids, and logs without bodies or ids beyond the path.

use serde_json::json;
use wiremock::matchers::{body_json, body_string, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{API_PATH, Gmail};
use crate::provider::{Provider, ProviderError};

fn thread_answer(id: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .set_body_json(json!({"id": id, "messages": [{"id": "m1", "labelIds": ["Label_1"]}]}))
}

#[tokio::test]
async fn modify_posts_the_label_ids_once_per_thread() {
    let gmail = Gmail::start().await;
    for id in ["t1", "t2"] {
        Mock::given(method("POST"))
            .and(path(format!("{API_PATH}/threads/{id}/modify")))
            .and(body_json(
                json!({"addLabelIds": ["Label_1"], "removeLabelIds": ["INBOX"]}),
            ))
            .respond_with(thread_answer(id))
            .expect(1)
            .mount(&gmail.server)
            .await;
    }
    gmail
        .provider()
        .modify_threads(
            &["t1".into(), "t2".into()],
            &["Label_1".into()],
            &["INBOX".into()],
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn trash_posts_an_empty_body() {
    let gmail = Gmail::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/threads/t1/trash")))
        .and(body_string(""))
        .respond_with(thread_answer("t1"))
        .expect(1)
        .mount(&gmail.server)
        .await;
    gmail
        .provider()
        .trash_threads(&["t1".into()])
        .await
        .unwrap();
}

#[tokio::test]
async fn a_404_is_not_found_and_a_429_is_transient() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/threads/gone/modify")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&gmail.server)
        .await;
    Mock::given(path(format!("{API_PATH}/threads/busy/trash")))
        .respond_with(ResponseTemplate::new(429))
        .mount(&gmail.server)
        .await;
    let provider = gmail.provider();
    let gone = provider
        .modify_threads(&["gone".into()], &[], &["INBOX".into()])
        .await;
    assert_eq!(gone, Err(ProviderError::NotFound));
    let busy = provider.trash_threads(&["busy".into()]).await.unwrap_err();
    assert!(matches!(busy, ProviderError::Transient { .. }), "{busy:?}");
}

#[tokio::test]
async fn a_400_for_a_label_is_permanent() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/threads/t1/modify")))
        .respond_with(
            ResponseTemplate::new(400).set_body_json(
                json!({"error": {"code": 400, "message": "Invalid label: Label_9"}}),
            ),
        )
        .mount(&gmail.server)
        .await;
    let error = gmail
        .provider()
        .modify_threads(&["t1".into()], &["Label_9".into()], &[])
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ProviderError::Permanent { message, .. } if message == "Gmail refused the request (HTTP 400)"),
        "{error:?}"
    );
    assert_eq!(error.detail(), Some("Invalid label: Label_9"));
}

#[tokio::test]
async fn ids_rail_would_not_put_in_a_path_or_too_many_labels_are_refused_before_sending() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    for bad in ["../labels", "t 1", ""] {
        let error = provider.trash_threads(&[bad.into()]).await.unwrap_err();
        assert!(matches!(error, ProviderError::Permanent { .. }), "{bad}");
    }
    let many: Vec<String> = (0..101).map(|i| format!("Label_{i}")).collect();
    assert!(
        provider
            .modify_threads(&["t1".into()], &many, &[])
            .await
            .is_err()
    );
    assert!(gmail.server.received_requests().await.unwrap().is_empty());
}

/// `logged` runs its own runtime, so this is a plain `#[test]` (as in `http_tests.rs`).
#[test]
fn the_log_names_the_method_and_path_only() {
    let (result, log) = crate::test_support::logged(async {
        let gmail = Gmail::start().await;
        Mock::given(path(format!("{API_PATH}/threads/t1/modify")))
            .respond_with(thread_answer("t1"))
            .mount(&gmail.server)
            .await;
        gmail
            .provider()
            .modify_threads(&["t1".into()], &["STARRED".into()], &[])
            .await
    });
    result.unwrap();
    assert!(
        log.contains("method=\"POST\"") || log.contains("method=POST"),
        "{log}"
    );
    assert!(!log.contains("STARRED"), "no body in the log: {log}");
}
