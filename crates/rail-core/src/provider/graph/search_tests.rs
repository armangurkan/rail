//! Server search and envelopes by id against fake Graph: the KQL phrase and the cap, one
//! batch of gets with immutable ids, a throttled item sent again alone, batches of twenty, and
//! a refused item.

use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{Graph, ROOT, message};
use super::search::{MESSAGE_FIELDS, kql};
use crate::provider::http::IMMUTABLE_IDS;
use crate::provider::{EnvelopePage, Provider, ProviderError};

const AT: &str = "2026-10-12T08:30:15Z";

fn ids(page: &EnvelopePage) -> Vec<&str> {
    page.envelopes
        .iter()
        .map(|e| e.remote_id.as_str())
        .collect()
}

#[test]
fn a_query_becomes_a_quoted_kql_phrase() {
    assert_eq!(kql("from:ann budget"), "\"from:ann budget\"");
    assert_eq!(kql(r#"say "hi" \ bye"#), r#""say \"hi\" \\ bye""#);
}

#[tokio::test]
async fn search_sends_the_phrase_asks_for_ids_and_caps_the_count() {
    let g = Graph::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages")))
        .and(query_param("$search", "\"from:ann \\\"budget\\\"\""))
        .and(query_param("$select", "id"))
        .and(query_param("$top", "1000"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"value": [{"id": "m1"}, {"id": "bad/id"}, {"id": "m2"}]})),
        )
        .expect(1)
        .mount(&g.server)
        .await;
    let found = g
        .provider()
        .search("from:ann \"budget\"", 5_000)
        .await
        .unwrap();
    assert_eq!(found, ["m1", "m2"]);
}

#[tokio::test]
async fn envelopes_come_from_one_batch_of_gets_with_immutable_ids() {
    let g = Graph::start().await;
    for id in ["m1", "m2"] {
        let at = format!("/me/messages/{id}");
        g.batch
            .answer("GET", &at, 200, message(id, "c1", "f-inbox", AT));
    }
    let asked: [String; 4] = ["m1".into(), "m2".into(), "m3".into(), "../x".into()];
    let page = g.provider().envelopes(&asked).await.unwrap();
    assert_eq!(ids(&page), ["m1", "m2"], "m3 is gone");
    assert_eq!(page.skipped, ["../x"]);
    assert_eq!(page.envelopes[0].folders, ["f-inbox"]);
    assert_eq!(g.batch.posts(), 1);
    let requests = g.batch.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request["method"], "GET");
        assert_eq!(request["headers"]["Prefer"], IMMUTABLE_IDS);
        let url = request["url"].as_str().unwrap();
        assert!(
            url.ends_with(&format!("?$select={MESSAGE_FIELDS}")),
            "{url}"
        );
    }
}

#[tokio::test]
async fn a_throttled_item_is_sent_again_alone_and_the_others_stand() {
    let g = Graph::start().await;
    g.batch.answer(
        "GET",
        "/me/messages/m1",
        200,
        message("m1", "c1", "f-inbox", AT),
    );
    g.batch.throttle_once("GET", "/me/messages/m2");
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages/m2")))
        .and(query_param("$select", MESSAGE_FIELDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(message("m2", "c2", "f-inbox", AT)))
        .expect(1)
        .mount(&g.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages/m1")))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&g.server)
        .await;
    let page = g
        .provider()
        .envelopes(&["m1".into(), "m2".into()])
        .await
        .unwrap();
    assert_eq!(ids(&page), ["m1", "m2"]);
    assert_eq!(g.batch.posts(), 1, "no second batch");
}

#[tokio::test]
async fn more_than_twenty_ids_go_twenty_to_a_batch() {
    let g = Graph::start().await;
    let asked: Vec<String> = (0..45).map(|i| format!("m{i}")).collect();
    let page = g.provider().envelopes(&asked).await.unwrap();
    assert!(
        page.envelopes.is_empty() && page.skipped.is_empty(),
        "all gone"
    );
    assert_eq!(g.batch.posts(), 3);
    assert_eq!(g.batch.requests().len(), 45);
}

#[tokio::test]
async fn a_refused_item_fails_the_page() {
    let g = Graph::start().await;
    let refused = json!({"error": {"code": "ServiceUnavailable", "message": "later"}});
    g.batch.answer("GET", "/me/messages/m1", 503, refused);
    let error = g.provider().envelopes(&["m1".into()]).await.unwrap_err();
    assert!(
        matches!(error, ProviderError::Transient { .. }),
        "{error:?}"
    );
}
