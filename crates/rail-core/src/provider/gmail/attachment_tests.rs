//! Attachment bytes against fake Gmail: the part id Rail keeps is turned into Gmail's
//! per-read `attachmentId` on every fetch.

use serde_json::{Value, json};
use wiremock::matchers::{method, path, path_regex};
use wiremock::{Mock, ResponseTemplate};

use super::attachment::find_part;
use super::fixtures::{API_PATH, Gmail, b64, full_json};
use super::wire::Part;
use super::*;
use crate::error::ErrorClass;
use crate::provider::Provider;

fn file_part(part_id: &str, body: Value) -> Value {
    json!({
        "partId": part_id, "mimeType": "application/pdf", "filename": "a.pdf",
        "headers": [], "body": body
    })
}

fn mixed(parts: Vec<Value>) -> Value {
    json!({
        "partId": "", "mimeType": "multipart/mixed", "filename": "", "headers": [],
        "body": {"size": 0}, "parts": parts
    })
}

async fn serve_attachment(gmail: &Gmail, attachment_id: &str, data: &[u8], hits: u64) {
    Mock::given(method("GET"))
        .and(path(format!(
            "{API_PATH}/messages/m1/attachments/{attachment_id}"
        )))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"size": data.len(), "data": b64(data)})),
        )
        .expect(hits)
        .mount(&gmail.server)
        .await;
}

async fn attachment_paths(gmail: &Gmail) -> Vec<String> {
    gmail
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .filter(|p| p.contains("/attachments/"))
        .collect()
}

#[tokio::test]
async fn a_part_is_fetched_by_its_attachment_id_not_its_part_id() {
    let gmail = Gmail::start().await;
    let payload = mixed(vec![file_part(
        "1",
        json!({"attachmentId": "ANGjdJ_real", "size": 3}),
    )]);
    gmail.mount_message(full_json("m1", payload)).await;
    serve_attachment(&gmail, "ANGjdJ_real", &[1, 2, 3], 1).await;
    let bytes = gmail
        .provider()
        .attachment(&"m1".to_string(), "1")
        .await
        .unwrap();
    assert_eq!(bytes, [1, 2, 3]);
}

#[tokio::test]
async fn padded_base64url_is_accepted() {
    let gmail = Gmail::start().await;
    let payload = mixed(vec![file_part("1", json!({"attachmentId": "A1"}))]);
    gmail.mount_message(full_json("m1", payload)).await;
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages/m1/attachments/A1")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"size": 4, "data": "AQIDBA=="})),
        )
        .mount(&gmail.server)
        .await;
    let bytes = gmail
        .provider()
        .attachment(&"m1".to_string(), "1")
        .await
        .unwrap();
    assert_eq!(bytes, [1, 2, 3, 4]);
}

#[tokio::test]
async fn an_inline_part_is_decoded_without_the_attachments_endpoint() {
    let gmail = Gmail::start().await;
    let payload = mixed(vec![file_part(
        "1",
        json!({"data": b64(&[9, 8, 7]), "size": 3}),
    )]);
    gmail.mount_message(full_json("m1", payload)).await;
    Mock::given(method("GET"))
        .and(path_regex(r"/attachments/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": "AA"})))
        .expect(0)
        .mount(&gmail.server)
        .await;
    let bytes = gmail
        .provider()
        .attachment(&"m1".to_string(), "1")
        .await
        .unwrap();
    assert_eq!(bytes, [9, 8, 7]);
    assert!(attachment_paths(&gmail).await.is_empty());
}

#[tokio::test]
async fn a_nested_part_id_is_found() {
    let gmail = Gmail::start().await;
    let inner = json!({
        "partId": "1", "mimeType": "multipart/alternative", "filename": "", "headers": [],
        "body": {"size": 0},
        "parts": [file_part("1.2", json!({"attachmentId": "nested_id", "size": 2}))]
    });
    gmail
        .mount_message(full_json("m1", mixed(vec![inner])))
        .await;
    serve_attachment(&gmail, "nested_id", &[5, 6], 1).await;
    let bytes = gmail
        .provider()
        .attachment(&"m1".to_string(), "1.2")
        .await
        .unwrap();
    assert_eq!(bytes, [5, 6]);
}

#[tokio::test]
async fn a_part_id_not_in_the_message_is_a_permanent_error_without_a_fetch() {
    let gmail = Gmail::start().await;
    let payload = mixed(vec![file_part("1", json!({"attachmentId": "real"}))]);
    gmail.mount_message(full_json("m1", payload)).await;
    let error = gmail
        .provider()
        .attachment(&"m1".to_string(), "7")
        .await
        .unwrap_err();
    assert_eq!(error, ProviderError::NotFound);
    assert!(attachment_paths(&gmail).await.is_empty());
}

#[tokio::test]
async fn a_part_with_neither_data_nor_attachment_id_is_a_permanent_error() {
    let gmail = Gmail::start().await;
    let payload = mixed(vec![file_part("1", json!({"size": 0}))]);
    gmail.mount_message(full_json("m1", payload)).await;
    let error = gmail
        .provider()
        .attachment(&"m1".to_string(), "1")
        .await
        .unwrap_err();
    assert_eq!(error, ProviderError::NotFound);
    assert!(attachment_paths(&gmail).await.is_empty());
}

#[tokio::test]
async fn an_unsafe_attachment_id_in_gmails_answer_is_never_requested() {
    let gmail = Gmail::start().await;
    let payload = mixed(vec![file_part(
        "1",
        json!({"attachmentId": "../../drafts/x?y=1"}),
    )]);
    gmail.mount_message(full_json("m1", payload)).await;
    let error = gmail
        .provider()
        .attachment(&"m1".to_string(), "1")
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::Permanent { .. }), "{error}");
    assert!(attachment_paths(&gmail).await.is_empty());
}

#[tokio::test]
async fn an_unsafe_message_id_is_refused_before_any_request() {
    let gmail = Gmail::start().await;
    let error = gmail
        .provider()
        .attachment(&"m1/../x".to_string(), "1")
        .await
        .unwrap_err();
    assert!(matches!(error, ProviderError::Permanent { .. }), "{error}");
    assert!(gmail.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_503_on_the_message_read_is_transient() {
    let gmail = Gmail::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages/m1")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&gmail.server)
        .await;
    let error = gmail
        .provider()
        .attachment(&"m1".to_string(), "1")
        .await
        .unwrap_err();
    assert_eq!(error.class(), ErrorClass::Transient, "{error:?}");
    assert!(attachment_paths(&gmail).await.is_empty());
}

#[tokio::test]
async fn a_404_on_the_message_read_is_not_found() {
    let gmail = Gmail::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages/m1")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&gmail.server)
        .await;
    let error = gmail
        .provider()
        .attachment(&"m1".to_string(), "1")
        .await
        .unwrap_err();
    assert_eq!(error, ProviderError::NotFound);
}

fn part(part_id: &str, children: Vec<Part>) -> Part {
    Part {
        part_id: part_id.to_string(),
        parts: children,
        ..Part::default()
    }
}

#[test]
fn find_part_walks_the_tree_and_stops_at_the_depth_limit() {
    let tree = part(
        "",
        vec![part("1", vec![part("1.1", vec![])]), part("2", vec![])],
    );
    assert_eq!(
        find_part(&tree, "1.1").map(|p| p.part_id.as_str()),
        Some("1.1")
    );
    assert_eq!(find_part(&tree, "2").map(|p| p.part_id.as_str()), Some("2"));
    assert!(find_part(&tree, "3").is_none());

    let mut deep = part("deep", vec![]);
    for _ in 0..40 {
        deep = part("x", vec![deep]);
    }
    assert!(find_part(&deep, "deep").is_none(), "too deep to follow");
}
