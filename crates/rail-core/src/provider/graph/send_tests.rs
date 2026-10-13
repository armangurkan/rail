//! Sending and the draft lookups against fake Graph: a send saves the draft then posts `send`
//! to it once and answers its id; a draft found by its Message-ID is the one sent, none created;
//! a source gone sends a new message; a refused send is not repeated; `sent_already` reads
//! `isDraft` by Message-ID and by the kept draft id; `draft_delete` and `draft_find`; Graph
//! sends its draft itself and carries forward originals.

use serde_json::json;
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use super::drafts_tests::{created, fields, found, mail, no_attachments, none_found};
use super::fixtures::{Graph, ROOT};
use crate::provider::http::IMMUTABLE_IDS;
use crate::provider::{Provider, ProviderError, SentReceipt};
use crate::store::drafts::DraftKind;

async fn send_to(g: &Graph, id: &str, status: u16, times: u64) {
    Mock::given(method("POST"))
        .and(path(format!("{ROOT}/me/messages/{id}/send")))
        .and(header("Prefer", IMMUTABLE_IDS))
        .respond_with(ResponseTemplate::new(status))
        .expect(times)
        .mount(&g.server)
        .await;
}

async fn new_draft_made(g: &Graph, id: &str, times: u64) {
    Mock::given(method("POST"))
        .and(path(format!("{ROOT}/me/messages")))
        .and(body_json(fields()))
        .respond_with(created(id))
        .expect(times)
        .mount(&g.server)
        .await;
}

fn receipt(id: &str) -> SentReceipt {
    SentReceipt {
        message_id: Some(id.into()),
        thread_id: None,
    }
}

#[tokio::test]
async fn send_saves_the_draft_then_posts_send_to_it_and_answers_its_id() {
    let g = Graph::start().await;
    none_found(&g).await;
    no_attachments(&g, "g-d1").await;
    new_draft_made(&g, "g-d1", 1).await;
    send_to(&g, "g-d1", 202, 1).await;
    let sent = g.provider().send(&mail(DraftKind::New, None, None)).await;
    assert_eq!(sent.unwrap(), receipt("g-d1"));
}

#[tokio::test]
async fn send_reuses_the_draft_its_message_id_finds_and_creates_none() {
    let g = Graph::start().await;
    found(&g, json!([{"id": "g-old", "isDraft": true}])).await;
    no_attachments(&g, "g-old").await;
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/g-old")))
        .and(body_json(fields()))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "g-old"})))
        .expect(1)
        .mount(&g.server)
        .await;
    new_draft_made(&g, "never", 0).await;
    send_to(&g, "g-old", 202, 1).await;
    let sent = g.provider().send(&mail(DraftKind::New, None, None)).await;
    assert_eq!(sent.unwrap(), receipt("g-old"));
}

#[tokio::test]
async fn a_send_whose_source_is_gone_goes_out_as_a_new_message() {
    let g = Graph::start().await;
    none_found(&g).await;
    no_attachments(&g, "g-n1").await;
    Mock::given(method("POST"))
        .and(path(format!("{ROOT}/me/messages/m-gone/createReply")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&g.server)
        .await;
    new_draft_made(&g, "g-n1", 1).await;
    send_to(&g, "g-n1", 202, 1).await;
    let sent = g
        .provider()
        .send(&mail(DraftKind::Reply, Some("m-gone"), None))
        .await;
    assert_eq!(sent.unwrap(), receipt("g-n1"));
}

#[tokio::test]
async fn a_refused_send_is_tried_once() {
    let g = Graph::start().await;
    no_attachments(&g, "g-d1").await;
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/g-d1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "g-d1"})))
        .mount(&g.server)
        .await;
    send_to(&g, "g-d1", 503, 1).await;
    let error = g
        .provider()
        .send(&mail(DraftKind::New, None, Some("g-d1")))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Transient { .. }),
        "unknown, for the worker's check: {error:?}"
    );
}

#[tokio::test]
async fn sent_already_reads_is_draft_by_message_id_and_by_the_kept_draft_id() {
    let g = Graph::start().await;
    let provider = g.provider();
    let fresh = mail(DraftKind::New, None, None);
    let held = mail(DraftKind::New, None, Some("g-d1"));
    none_found(&g).await;
    assert!(!provider.sent_already(&fresh).await.unwrap());
    g.reset().await;
    found(&g, json!([{"id": "g1", "isDraft": true}])).await;
    assert!(
        !provider.sent_already(&fresh).await.unwrap(),
        "only a draft"
    );
    g.reset().await;
    found(
        &g,
        json!([{"id": "g1", "isDraft": true}, {"id": "g2", "isDraft": false}]),
    )
    .await;
    assert!(provider.sent_already(&fresh).await.unwrap());
    for (answer, sent) in [
        (
            ResponseTemplate::new(200).set_body_json(json!({"id": "g-d1", "isDraft": false})),
            true,
        ),
        (
            ResponseTemplate::new(200).set_body_json(json!({"id": "g-d1", "isDraft": true})),
            false,
        ),
        (ResponseTemplate::new(404), false),
    ] {
        g.reset().await;
        none_found(&g).await;
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}/me/messages/g-d1")))
            .and(query_param("$select", "id,isDraft"))
            .respond_with(answer)
            .expect(1)
            .mount(&g.server)
            .await;
        assert_eq!(provider.sent_already(&held).await.unwrap(), sent);
        g.server.verify().await;
    }
}

async fn kind_of(g: &Graph, id: &str, answer: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages/{id}")))
        .and(query_param("$select", "id,isDraft"))
        .respond_with(answer)
        .expect(1)
        .mount(&g.server)
        .await;
}

async fn deleted(g: &Graph, id: &str, status: u16, times: u64) {
    Mock::given(method("DELETE"))
        .and(path(format!("{ROOT}/me/messages/{id}")))
        .and(header("Prefer", IMMUTABLE_IDS))
        .respond_with(ResponseTemplate::new(status))
        .expect(times)
        .mount(&g.server)
        .await;
}

#[tokio::test]
async fn draft_delete_of_a_draft_reads_it_then_deletes_it() {
    let g = Graph::start().await;
    kind_of(
        &g,
        "g-d1",
        ResponseTemplate::new(200).set_body_json(json!({"id": "g-d1", "isDraft": true})),
    )
    .await;
    deleted(&g, "g-d1", 204, 1).await;
    g.provider().draft_delete("g-d1").await.unwrap();
}

#[tokio::test]
async fn draft_delete_of_a_sent_message_deletes_nothing() {
    let g = Graph::start().await;
    kind_of(
        &g,
        "g-s1",
        ResponseTemplate::new(200).set_body_json(json!({"id": "g-s1", "isDraft": false})),
    )
    .await;
    deleted(&g, "g-s1", 204, 0).await;
    g.provider().draft_delete("g-s1").await.unwrap();
}

#[tokio::test]
async fn draft_delete_of_a_message_that_is_gone_is_done_without_a_delete() {
    let g = Graph::start().await;
    kind_of(&g, "g-gone", ResponseTemplate::new(404)).await;
    deleted(&g, "g-gone", 204, 0).await;
    g.provider().draft_delete("g-gone").await.unwrap();
}

#[tokio::test]
async fn draft_delete_of_a_draft_that_vanishes_before_the_delete_is_done() {
    let g = Graph::start().await;
    kind_of(
        &g,
        "g-d2",
        ResponseTemplate::new(200).set_body_json(json!({"id": "g-d2", "isDraft": true})),
    )
    .await;
    deleted(&g, "g-d2", 404, 1).await;
    g.provider().draft_delete("g-d2").await.unwrap();
}

#[tokio::test]
async fn draft_delete_refuses_an_id_before_any_request() {
    let g = Graph::start().await;
    let error = g.provider().draft_delete("bad/id").await.unwrap_err();
    assert!(matches!(error, ProviderError::Permanent { .. }));
    assert!(g.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn draft_find_answers_a_draft_and_nothing_else() {
    let g = Graph::start().await;
    for (id, answer) in [
        (
            "g-d1",
            ResponseTemplate::new(200).set_body_json(json!({"id": "g-d1", "isDraft": true})),
        ),
        (
            "g-s1",
            ResponseTemplate::new(200).set_body_json(json!({"id": "g-s1", "isDraft": false})),
        ),
        ("g-gone", ResponseTemplate::new(404)),
    ] {
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}/me/messages/{id}")))
            .and(query_param("$select", "id,isDraft"))
            .respond_with(answer)
            .mount(&g.server)
            .await;
    }
    let provider = g.provider();
    assert_eq!(
        provider.draft_find("g-d1").await.unwrap(),
        Some("g-d1".into())
    );
    assert_eq!(
        provider.draft_find("g-s1").await.unwrap(),
        None,
        "sent, not a draft"
    );
    assert_eq!(provider.draft_find("g-gone").await.unwrap(), None);
    assert!(provider.draft_find("bad/id").await.is_err());
}

#[test]
fn graph_sends_its_draft_itself_and_carries_forward_originals() {
    let provider = super::GraphProvider::new(super::fixtures::api_at("http://127.0.0.1:9/v1.0/"));
    assert!(provider.send_consumes_draft());
    assert!(provider.forward_carries_originals());
}
