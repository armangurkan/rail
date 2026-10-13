//! A throttled batch item: how long it waits (its `Retry-After`, capped at
//! `MAX_ITEM_RETRY_AFTER`, a second without one), and the wait itself under a paused clock.

use std::time::Duration;

use serde_json::{Value, json};
use wiremock::matchers::{method, path};
use wiremock::{Mock, ResponseTemplate};

use super::batch::{ITEM_RETRY_DEFAULT, item_wait};
use super::fixtures::{Graph, ROOT, api_untimed};
use super::{GraphProvider, MAX_ITEM_RETRY_AFTER};
use crate::provider::{Change, FolderKind, Provider, TargetMessage, ThreadTarget};

fn headers(value: Value) -> serde_json::Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn inbox(ids: &[&str]) -> ThreadTarget {
    ThreadTarget {
        thread_id: "c1".into(),
        messages: ids
            .iter()
            .zip(1..)
            .map(|(id, date)| TargetMessage {
                id: (*id).into(),
                folder: Some(FolderKind::Inbox),
                folder_remote: Some("f-inbox".into()),
                unread: true,
                starred: false,
                date,
            })
            .collect(),
    }
}

#[test]
fn an_items_wait_is_its_retry_after_capped_or_one_second() {
    assert_eq!(item_wait(&headers(json!({}))), ITEM_RETRY_DEFAULT);
    assert_eq!(ITEM_RETRY_DEFAULT, Duration::from_secs(1));
    assert_eq!(
        item_wait(&headers(json!({"Retry-After": "5"}))),
        Duration::from_secs(5)
    );
    assert_eq!(
        item_wait(&headers(json!({"retry-after": "3600"}))),
        MAX_ITEM_RETRY_AFTER
    );
    assert_eq!(MAX_ITEM_RETRY_AFTER, Duration::from_secs(30));
    assert_eq!(
        item_wait(&headers(json!({"Retry-After": 7}))),
        ITEM_RETRY_DEFAULT
    );
    assert_eq!(
        item_wait(&headers(json!({"Retry-After": "soon"}))),
        ITEM_RETRY_DEFAULT
    );
}

#[tokio::test(start_paused = true)]
async fn a_throttled_item_waits_for_its_retry_after_and_never_more_than_thirty_seconds() {
    let g = Graph::start().await;
    g.batch
        .throttle_with("PATCH", "/me/messages/m1", Some("3600"));
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/m1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "m1"})))
        .expect(1)
        .mount(&g.server)
        .await;
    let provider = GraphProvider::new(api_untimed(&g.base()));
    let started = tokio::time::Instant::now();
    provider
        .apply(&[inbox(&["m1"])], &Change::Read { on: true })
        .await
        .unwrap();
    let waited = started.elapsed();
    assert!(waited >= MAX_ITEM_RETRY_AFTER, "waited {waited:?}");
    assert!(waited < Duration::from_secs(3_600), "capped: {waited:?}");
}

#[tokio::test(start_paused = true)]
async fn a_throttled_item_refused_on_its_resend_is_skipped_and_the_others_stand() {
    let g = Graph::start().await;
    for id in ["m1", "m2", "m3"] {
        g.batch.answer(
            "PATCH",
            &format!("/me/messages/{id}"),
            200,
            json!({"id": id}),
        );
    }
    g.batch.throttle_with("PATCH", "/me/messages/m2", None);
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/m2")))
        .respond_with(
            ResponseTemplate::new(403)
                .set_body_json(json!({"error": {"code": "ErrorAccessDenied", "message": "no"}})),
        )
        .expect(1)
        .mount(&g.server)
        .await;
    let provider = GraphProvider::new(api_untimed(&g.base()));
    provider
        .apply(&[inbox(&["m1", "m2", "m3"])], &Change::Read { on: true })
        .await
        .unwrap();
    assert_eq!(g.batch.posts(), 1);
}

#[tokio::test(start_paused = true)]
async fn throttled_items_of_a_chunk_wait_once_for_the_longest_capped_retry_after() {
    let g = Graph::start().await;
    for (id, after) in [("m1", "5"), ("m2", "20"), ("m3", "60")] {
        g.batch
            .throttle_with("PATCH", &format!("/me/messages/{id}"), Some(after));
        Mock::given(method("PATCH"))
            .and(path(format!("{ROOT}/me/messages/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
            .expect(1)
            .mount(&g.server)
            .await;
    }
    let provider = GraphProvider::new(api_untimed(&g.base()));
    let started = tokio::time::Instant::now();
    provider
        .apply(&[inbox(&["m1", "m2", "m3"])], &Change::Read { on: true })
        .await
        .unwrap();
    assert_eq!(started.elapsed(), MAX_ITEM_RETRY_AFTER);
}
