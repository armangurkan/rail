//! Mark read against fake Graph: one PATCH per stored message through `$batch`, twenty to a
//! batch, a gone message counted as done, a throttled one sent again alone, a refused PATCH,
//! and every other change refused.

use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::errors::CONSENT_MESSAGE;
use super::fixtures::{Graph, ROOT};
use crate::ops::M365_NOT_YET;
use crate::provider::{Provider, ProviderError, STARRED_LABEL, ThreadTarget, UNREAD_LABEL};

fn target(thread: &str, messages: &[&str]) -> ThreadTarget {
    ThreadTarget {
        thread_id: thread.into(),
        message_ids: messages.iter().map(|m| (*m).to_string()).collect(),
    }
}

#[tokio::test]
async fn mark_read_patches_every_stored_message_twenty_to_a_batch() {
    let g = Graph::start().await;
    let many: Vec<String> = (0..25).map(|n| format!("m{n}")).collect();
    let ids: Vec<&str> = many.iter().map(String::as_str).collect();
    for id in &many {
        let at = format!("/me/messages/{id}");
        g.batch
            .answer("PATCH", &at, 200, json!({"id": id, "isRead": true}));
    }
    let threads = [target("c1", &ids[..5]), target("c2", &ids[5..])];
    g.provider()
        .modify_threads(&threads, &[], &[UNREAD_LABEL.into()])
        .await
        .unwrap();
    assert_eq!(g.batch.posts(), 2);
    let requests = g.batch.requests();
    assert_eq!(requests.len(), 25);
    for request in &requests {
        assert_eq!(request["method"], "PATCH");
        assert_eq!(request["body"], json!({"isRead": true}));
        assert_eq!(request["headers"]["Content-Type"], "application/json");
    }
}

#[tokio::test]
async fn a_gone_message_counts_as_done_and_a_throttled_one_is_sent_alone() {
    let g = Graph::start().await;
    g.batch.throttle_once("PATCH", "/me/messages/m2");
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/m2")))
        .and(body_json(json!({"isRead": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "m2", "isRead": true})))
        .expect(1)
        .mount(&g.server)
        .await;
    // m1 has no answer: the fake batch answers 404, which is done.
    g.provider()
        .modify_threads(&[target("c1", &["m1", "m2"])], &[], &[UNREAD_LABEL.into()])
        .await
        .unwrap();
    assert_eq!(g.batch.posts(), 1);
}

#[tokio::test]
async fn a_refused_patch_fails_the_call() {
    let g = Graph::start().await;
    let refused = json!({"error": {"code": "ErrorAccessDenied", "message": "no"}});
    g.batch.answer("PATCH", "/me/messages/m1", 403, refused);
    let error = g
        .provider()
        .modify_threads(&[target("c1", &["m1"])], &[], &[UNREAD_LABEL.into()])
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), CONSENT_MESSAGE);
}

#[tokio::test]
async fn every_other_change_and_trash_are_not_supported_yet() {
    let g = Graph::start().await;
    let provider = g.provider();
    let thread = [target("c1", &["m1"])];
    let changes = [
        (vec![], vec!["INBOX".to_string()]),
        (vec![STARRED_LABEL.to_string()], vec![]),
        (vec![UNREAD_LABEL.to_string()], vec![]),
        (vec!["f-arch".to_string()], vec!["INBOX".to_string()]),
    ];
    for (add, remove) in changes {
        let error = provider
            .modify_threads(&thread, &add, &remove)
            .await
            .unwrap_err();
        assert_eq!(error, ProviderError::permanent(M365_NOT_YET));
    }
    let trash = provider.trash_threads(&thread).await.unwrap_err();
    assert_eq!(trash, ProviderError::permanent(M365_NOT_YET));
    assert_eq!(g.batch.posts(), 0);
}
