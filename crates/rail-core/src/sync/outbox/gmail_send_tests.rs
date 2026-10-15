//! Send recovery against fake Gmail with an unsent draft sharing the message's Message-ID.

use std::sync::Arc;

use serde_json::json;
use tokio::sync::Notify;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use super::sending_tests::{Harness, queued_draft};
use crate::provider::gmail::fixtures::{API_PATH, Gmail};
use crate::store::outbox::OutboxState;

async fn mount_unsent_draft(gmail: &Gmail) {
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages")))
        .and(query_param("q", "rfc822msgid:<d59@example.com>"))
        .respond_with(|request: &wiremock::Request| {
            let sent_only = request
                .url
                .query_pairs()
                .any(|(key, value)| key == "labelIds" && value == "SENT");
            let messages = if sent_only {
                json!([])
            } else {
                json!([{"id": "m-draft", "threadId": "t1"}])
            };
            ResponseTemplate::new(200).set_body_json(json!({"messages": messages}))
        })
        .expect(1)
        .mount(&gmail.server)
        .await;
}

#[tokio::test]
async fn a_failed_send_retries_even_when_gmail_holds_the_unsent_draft() {
    let h = Harness::new().await;
    let gmail = Gmail::start().await;
    let (draft, row) = queued_draft(&h, 59, vec![]).await;
    h.with_store(|s| s.draft_set_remote(draft, "r1", "m-draft").unwrap());
    let worker = h
        .rig
        .worker_on(Arc::new(gmail.provider()), None, Arc::new(Notify::new()));
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/messages/send")))
        .respond_with(ResponseTemplate::new(429))
        .expect(1)
        .mount(&gmail.server)
        .await;
    worker.step().await;
    let pending = h.row(row).await.unwrap();
    assert_eq!(pending.state, OutboxState::Pending);
    assert_eq!(pending.attempts, 1);
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_some());
    gmail.server.verify().await;
    gmail.server.reset().await;
    mount_unsent_draft(&gmail).await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/messages/send")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "m-sent", "threadId": "t1"})),
        )
        .expect(1)
        .mount(&gmail.server)
        .await;
    h.advance_past(pending.not_before).await;
    worker.step().await;
    gmail.server.verify().await;
    assert!(h.row(row).await.is_none());
    assert!(h.with_store(|s| s.draft(draft).unwrap()).is_none());
    assert!(h.failures().is_empty());
}
