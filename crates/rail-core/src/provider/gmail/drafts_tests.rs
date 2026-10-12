//! `drafts.create`, `drafts.update`, `drafts.delete` and the message-to-draft lookup against
//! fake Gmail.

use serde_json::json;
use wiremock::matchers::{body_json, method, path, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{API_PATH, Gmail};
use super::{DRAFT_LIST_PAGE, MAX_DRAFT_LIST_PAGES};
use crate::provider::{DraftIds, Provider, ProviderError};

fn ids(draft: &str, message: &str) -> DraftIds {
    DraftIds {
        draft_id: draft.into(),
        message_id: message.into(),
    }
}

fn answer(draft: &str, message: &str) -> serde_json::Value {
    json!({"id": draft, "message": {"id": message, "threadId": "t1"}})
}

#[tokio::test]
async fn a_create_posts_the_raw_message_in_its_thread_once() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/drafts")))
        .and(body_json(
            json!({"message": {"raw": "SGVsbG8", "threadId": "t1"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer("r1", "m1")))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let created = provider.draft_create(b"Hello", Some("t1")).await.unwrap();
    assert_eq!(created, ids("r1", "m1"));
    assert_eq!(provider.api.units_taken(), 10);

    gmail.server.reset().await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/drafts")))
        .and(body_json(json!({"message": {"raw": "SGVsbG8"}})))
        .respond_with(ResponseTemplate::new(503))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let error = provider.draft_create(b"Hello", None).await.unwrap_err();
    assert!(
        matches!(error, ProviderError::Transient { .. }),
        "a create is tried once: {error:?}"
    );
}

#[tokio::test]
async fn an_update_puts_the_message_and_a_404_is_not_found() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    Mock::given(method("PUT"))
        .and(path(format!("{API_PATH}/drafts/r1")))
        .and(body_json(
            json!({"id": "r1", "message": {"raw": "SGVsbG8", "threadId": "t1"}}),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_json(answer("r1", "m2")))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let updated = provider
        .draft_update("r1", b"Hello", Some("t1"))
        .await
        .unwrap();
    assert_eq!(updated, ids("r1", "m2"));
    assert_eq!(provider.api.units_taken(), 15);

    gmail.server.reset().await;
    Mock::given(method("PUT"))
        .and(path(format!("{API_PATH}/drafts/r1")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let error = provider
        .draft_update("r1", b"Hello", None)
        .await
        .unwrap_err();
    assert_eq!(error, ProviderError::NotFound);
}

#[tokio::test]
async fn a_delete_is_a_delete_and_a_404_is_not_found() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    Mock::given(method("DELETE"))
        .and(path(format!("{API_PATH}/drafts/r1")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&gmail.server)
        .await;
    provider.draft_delete("r1").await.unwrap();
    assert_eq!(provider.api.units_taken(), 10);
    let requests = gmail.server.received_requests().await.unwrap();
    assert!(requests[0].body.is_empty(), "a delete sends no body");

    gmail.server.reset().await;
    Mock::given(method("DELETE"))
        .and(path(format!("{API_PATH}/drafts/r1")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&gmail.server)
        .await;
    assert_eq!(
        provider.draft_delete("r1").await.unwrap_err(),
        ProviderError::NotFound
    );
}

fn page(drafts: &[(&str, &str)], next: Option<&str>) -> serde_json::Value {
    let drafts: Vec<_> = drafts
        .iter()
        .map(|(d, m)| json!({"id": d, "message": {"id": m, "threadId": "t"}}))
        .collect();
    let mut list = json!({ "drafts": drafts });
    if let Some(next) = next {
        list["nextPageToken"] = json!(next);
    }
    list
}

#[tokio::test]
async fn find_walks_the_pages_and_stops_at_ten() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    let max = DRAFT_LIST_PAGE.to_string();
    let drafts_path = format!("{API_PATH}/drafts");
    Mock::given(method("GET"))
        .and(path(drafts_path.clone()))
        .and(query_param("maxResults", max.as_str()))
        .and(query_param_is_missing("pageToken"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&[("r1", "m1")], Some("p2"))))
        .expect(1)
        .mount(&gmail.server)
        .await;
    Mock::given(method("GET"))
        .and(path(drafts_path.clone()))
        .and(query_param("pageToken", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&[("r2", "m2")], Some("p3"))))
        .expect(1)
        .mount(&gmail.server)
        .await;
    Mock::given(method("GET"))
        .and(path(drafts_path.clone()))
        .and(query_param("pageToken", "p3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&[("r3", "m3")], Some("p4"))))
        .expect(1)
        .mount(&gmail.server)
        .await;
    assert_eq!(
        provider.draft_find("m3").await.unwrap(),
        Some("r3".to_string())
    );
    assert_eq!(provider.api.units_taken(), 15, "5 units a page");

    gmail.server.reset().await;
    Mock::given(method("GET"))
        .and(path(drafts_path))
        .respond_with(ResponseTemplate::new(200).set_body_json(page(&[], Some("again"))))
        .expect(u64::try_from(MAX_DRAFT_LIST_PAGES).unwrap())
        .mount(&gmail.server)
        .await;
    assert_eq!(provider.draft_find("m9").await.unwrap(), None);
}

#[tokio::test]
async fn unsafe_ids_are_refused_before_any_request() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    let errors = [
        provider.draft_update("r/1", b"x", None).await.unwrap_err(),
        provider.draft_delete("../x").await.unwrap_err(),
        provider.draft_find("m 1").await.map(|_| ()).unwrap_err(),
        provider
            .draft_create(b"x", Some("t 1"))
            .await
            .map(|_| ())
            .unwrap_err(),
        provider
            .draft_update("r1", b"x", Some("t 1"))
            .await
            .map(|_| ())
            .unwrap_err(),
    ];
    for error in errors {
        assert!(
            matches!(error, ProviderError::Permanent { .. }),
            "{error:?}"
        );
    }
    assert!(gmail.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_create_answer_rail_cannot_read_is_a_permanent_error_that_says_so() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/drafts")))
        .respond_with(ResponseTemplate::new(200).set_body_string("{}"))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let error = provider.draft_create(b"Hello", None).await.unwrap_err();
    assert_eq!(
        error,
        ProviderError::permanent("Gmail saved the draft but its answer was unreadable")
    );
}
