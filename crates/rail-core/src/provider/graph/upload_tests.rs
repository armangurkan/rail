//! Upload sessions against fake Graph: only Outlook's (or the API's own) host takes the bytes;
//! a large attachment goes up in 3,932,160-byte chunks with `Content-Range`, never with
//! `Authorization` or `Prefer`; a failed chunk is sent again from where the session said it was;
//! three failures are transient and the row's retry starts a new session; an attachment Outlook
//! already shows is not uploaded again; a finished session reads the size Outlook lists for
//! the file from one listing; the answers' reading.

use serde_json::json;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::drafts_tests::mail;
use super::fixtures::{Graph, ROOT, api_at};
use super::reconcile::FILE_ATTACHMENT;
use super::upload::{Progress, progress};
use super::{SMALL_ATTACHMENT_BYTES, UPLOAD_CHUNK};
use crate::provider::http::ChunkAnswer;
use crate::provider::{OutgoingAttachment, Provider, ProviderError};
use crate::store::drafts::DraftKind;

const TOTAL: usize = 8_000_000;

fn big() -> OutgoingAttachment {
    OutgoingAttachment {
        name: "big.bin".into(),
        mime: "application/octet-stream".into(),
        bytes: vec![7; TOTAL],
        carried: None,
        listed: None,
    }
}

fn range(start: usize, end: usize) -> String {
    format!("bytes {start}-{end}/{TOTAL}")
}

fn more(next: usize) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({"nextExpectedRanges": [format!("{next}-")]}))
}

/// The draft g-d1 (held, patched), listing `value`, and the session `createUploadSession`
/// makes, at `/upload/s1` on the fake server.
async fn session(g: &Graph, value: serde_json::Value) {
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/g-d1")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "g-d1"})))
        .mount(&g.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages/g-d1/attachments")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": value})))
        .mount(&g.server)
        .await;
    let url = format!("{}/upload/s1?authtoken=secret", g.server.uri());
    Mock::given(method("POST"))
        .and(path(format!(
            "{ROOT}/me/messages/g-d1/attachments/createUploadSession"
        )))
        .and(body_json(
            json!({"AttachmentItem": {"attachmentType": "file", "name": "big.bin",
                               "size": TOTAL, "contentType": "application/octet-stream"}}),
        ))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"uploadUrl": url})))
        .mount(&g.server)
        .await;
}

async fn chunk(g: &Graph, content_range: String, answer: ResponseTemplate) {
    Mock::given(method("PUT"))
        .and(path("/upload/s1"))
        .and(header("Content-Range", content_range.as_str()))
        .respond_with(answer)
        .mount(&g.server)
        .await;
}

fn with_big() -> crate::provider::OutgoingMail {
    let mut draft = mail(DraftKind::New, None, Some("g-d1"));
    draft.outgoing.attachments = vec![big()];
    draft
}

async fn puts(g: &Graph) -> Vec<wiremock::Request> {
    g.server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "PUT")
        .collect()
}

async fn sessions(g: &Graph) -> usize {
    g.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/createUploadSession"))
        .count()
}

#[test]
fn only_outlooks_hosts_and_the_apis_own_take_the_bytes() {
    let api = api_at("https://graph.microsoft.com/v1.0/");
    for good in [
        "https://outlook.office.com/api/v2.0/Users('a')/Messages('b')/AttachmentSessions('c')?authtoken=x",
        "https://outlook.office365.com/api/beta/session?authtoken=x",
        "https://graph.microsoft.com/v1.0/upload?x=1",
    ] {
        assert!(api.upload_url(good).is_ok(), "{good}");
    }
    for bad in [
        "http://outlook.office.com/upload",
        "https://evil.example/upload",
        "https://outlook.office.com.evil.example/upload",
        "https://user:pw@outlook.office.com/upload",
        "not a url",
    ] {
        let error = api.upload_url(bad).unwrap_err();
        assert!(matches!(error, ProviderError::Permanent { .. }), "{bad}");
        assert!(
            !format!("{error:?}").contains("evil"),
            "the address stays out of the error"
        );
    }
}

#[test]
fn a_chunks_answer_says_done_where_next_or_unreadable() {
    let answer = |status: u16, body: &str| ChunkAnswer {
        status,
        body: body.as_bytes().to_vec(),
    };
    assert_eq!(progress(&answer(201, ""), 0, TOTAL), Progress::Done);
    let next = r#"{"nextExpectedRanges":["3932160-"]}"#;
    assert_eq!(
        progress(&answer(200, next), 0, TOTAL),
        Progress::At(UPLOAD_CHUNK)
    );
    let pascal = r#"{"NextExpectedRanges":["3932160-"]}"#;
    assert_eq!(
        progress(&answer(200, pascal), 0, TOTAL),
        Progress::At(UPLOAD_CHUNK)
    );
    let ranged = r#"{"nextExpectedRanges":["3932160-7999999"]}"#;
    assert_eq!(
        progress(&answer(200, ranged), 0, TOTAL),
        Progress::At(UPLOAD_CHUNK)
    );
    for (body, offset) in [
        (r#"{"nextExpectedRanges":["0-"]}"#, 0),
        (r#"{"nextExpectedRanges":["8000000-"]}"#, 0),
        (r#"{"nextExpectedRanges":[]}"#, 0),
        ("not json", 0),
    ] {
        assert_eq!(
            progress(&answer(200, body), offset, TOTAL),
            Progress::Unreadable,
            "{body}"
        );
    }
}

#[tokio::test]
async fn a_large_attachment_goes_up_in_chunks_with_content_range_and_no_token() {
    let g = Graph::start().await;
    session(&g, json!([])).await;
    chunk(&g, range(0, 3_932_159), more(3_932_160)).await;
    chunk(&g, range(3_932_160, 7_864_319), more(7_864_320)).await;
    chunk(&g, range(7_864_320, 7_999_999), ResponseTemplate::new(201)).await;
    g.provider().draft_save(&with_big()).await.unwrap();
    let sent = puts(&g).await;
    let sizes: Vec<usize> = sent.iter().map(|r| r.body.len()).collect();
    assert_eq!(sizes, [3_932_160, 3_932_160, 135_680]);
    for r in &sent {
        assert!(
            !r.headers.contains_key("authorization"),
            "the session carries its own credential"
        );
        assert!(!r.headers.contains_key("prefer"));
    }
    const { assert!(TOTAL >= SMALL_ATTACHMENT_BYTES) };
    let small_posts = g
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "POST" && r.url.path().ends_with("/g-d1/attachments"))
        .count();
    assert_eq!(small_posts, 0, "a large one is never posted whole");
}

#[tokio::test]
async fn a_failed_chunk_is_sent_again_from_where_the_session_was() {
    let g = Graph::start().await;
    session(&g, json!([])).await;
    chunk(&g, range(0, 3_932_159), more(3_932_160)).await;
    Mock::given(method("PUT"))
        .and(path("/upload/s1"))
        .and(header(
            "Content-Range",
            range(3_932_160, 7_864_319).as_str(),
        ))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&g.server)
        .await;
    chunk(&g, range(3_932_160, 7_864_319), more(7_864_320)).await;
    chunk(&g, range(7_864_320, 7_999_999), ResponseTemplate::new(201)).await;
    g.provider().draft_save(&with_big()).await.unwrap();
    let ranges: Vec<String> = puts(&g)
        .await
        .iter()
        .map(|r| r.headers["content-range"].to_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ranges,
        [
            range(0, 3_932_159),
            range(3_932_160, 7_864_319),
            range(3_932_160, 7_864_319),
            range(7_864_320, 7_999_999)
        ]
    );
    assert_eq!(sessions(&g).await, 1);
}

#[tokio::test]
async fn three_failed_tries_are_transient_and_the_retry_uploads_through_a_new_session() {
    let g = Graph::start().await;
    session(&g, json!([])).await;
    chunk(&g, range(0, 3_932_159), more(3_932_160)).await;
    chunk(&g, range(3_932_160, 7_864_319), ResponseTemplate::new(500)).await;
    let error = g.provider().draft_save(&with_big()).await.unwrap_err();
    assert!(
        matches!(error, ProviderError::Transient { .. }),
        "{error:?}"
    );
    assert_eq!(
        puts(&g).await.len(),
        1 + 3,
        "one good chunk, three tries of the next"
    );
    assert!(!format!("{error:?}").contains("authtoken"));
    // The row's retry: the unfinished session left nothing on the draft, so a new one starts.
    g.reset().await;
    session(&g, json!([])).await;
    chunk(&g, range(0, 3_932_159), more(3_932_160)).await;
    chunk(&g, range(3_932_160, 7_864_319), more(7_864_320)).await;
    chunk(&g, range(7_864_320, 7_999_999), ResponseTemplate::new(201)).await;
    g.provider().draft_save(&with_big()).await.unwrap();
    assert_eq!(sessions(&g).await, 1, "a new session on the retry");
}

#[tokio::test]
async fn an_upload_whose_last_answer_was_lost_is_not_uploaded_again() {
    let g = Graph::start().await;
    let done = json!({"@odata.type": FILE_ATTACHMENT, "id": "att-big", "name": "big.bin",
                      "size": TOTAL, "contentType": "application/octet-stream", "isInline": false});
    session(&g, json!([done])).await;
    g.provider().draft_save(&with_big()).await.unwrap();
    assert_eq!(sessions(&g).await, 0, "Outlook shows it: no second copy");
    assert!(puts(&g).await.is_empty());
}

#[tokio::test]
async fn a_finished_session_reads_the_size_outlook_lists_from_one_listing() {
    let g = Graph::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages/g-d1/attachments")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&g.server)
        .await;
    let listed = json!({"@odata.type": FILE_ATTACHMENT, "id": "att-big", "name": "big.bin",
                        "size": TOTAL + 177, "contentType": "application/octet-stream",
                        "isInline": false});
    session(&g, json!([listed])).await;
    chunk(&g, range(0, 3_932_159), more(3_932_160)).await;
    chunk(&g, range(3_932_160, 7_864_319), more(7_864_320)).await;
    chunk(&g, range(7_864_320, 7_999_999), ResponseTemplate::new(201)).await;
    let saved = g.provider().draft_save(&with_big()).await.unwrap();
    assert_eq!(saved.listed, [Some(TOTAL + 177)], "the size Outlook lists");
    let listings = g
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "GET" && r.url.path().ends_with("/attachments"))
        .count();
    assert_eq!(
        listings, 2,
        "the reconcile's listing, and one after the upload"
    );
}
