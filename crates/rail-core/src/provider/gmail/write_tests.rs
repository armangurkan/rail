//! The Gmail adapter's side of the writing trait: `send` and `draft_save` build the message with
//! the MIME builder and make exactly the requests the raw-bytes calls made (`raw`, `threadId`,
//! the draft id), a thread or draft gone at Gmail falls back as the worker did, `sent_already`
//! is the `rfc822msgid:` search, a send never consumes a draft, and a carried attachment (which
//! has no bytes) is refused before any request.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use mail_parser::MessageParser;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{API_PATH, Gmail};
use crate::provider::{
    Carried, DraftIds, Mailbox, Outgoing, OutgoingAttachment, OutgoingMail, Provider,
    ProviderError, SentReceipt,
};
use crate::store::drafts::DraftKind;

fn mail(thread: Option<&str>, remote: Option<&str>) -> OutgoingMail {
    OutgoingMail {
        outgoing: Outgoing {
            from_name: Some("Dana".into()),
            from: "me@example.com".into(),
            to: vec![Mailbox {
                name: Some("Sam".into()),
                address: "sam@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![],
            subject: "Plans".into(),
            message_id: "<w1@example.com>".into(),
            in_reply_to: None,
            references: vec![],
            body: "see you\n".into(),
            attachments: vec![],
        },
        kind: DraftKind::New,
        source: None,
        remote: remote.map(|id| DraftIds {
            draft_id: id.into(),
            message_id: "m-old".into(),
        }),
        thread: thread.map(str::to_string),
    }
}

/// The JSON bodies of the `verb` requests fake Gmail took at `at` (under the API root).
async fn bodies(gmail: &Gmail, verb: &str, at: &str) -> Vec<Value> {
    let wanted = format!("{API_PATH}/{at}");
    gmail
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == verb && r.url.path() == wanted)
        .map(|r| serde_json::from_slice(&r.body).unwrap())
        .collect()
}

fn keys(value: &Value) -> Vec<String> {
    let mut keys: Vec<String> = value.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    keys
}

/// The Message-ID (without brackets) and subject of a base64url `raw`.
fn headers_of(raw: &Value) -> (String, String) {
    let bytes = URL_SAFE_NO_PAD.decode(raw.as_str().unwrap()).unwrap();
    let parsed = MessageParser::default().parse(&bytes).unwrap();
    (
        parsed.message_id().unwrap().to_string(),
        parsed.subject().unwrap().to_string(),
    )
}

fn built() -> (String, String) {
    ("w1@example.com".into(), "Plans".into())
}

fn ids(draft: &str, message: &str) -> DraftIds {
    DraftIds {
        draft_id: draft.into(),
        message_id: message.into(),
    }
}

#[tokio::test]
async fn send_posts_the_built_message_in_its_thread_and_once_more_without_a_gone_thread() {
    let gmail = Gmail::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/messages/send")))
        .and(body_partial_json(json!({"threadId": "t-gone"})))
        .respond_with(ResponseTemplate::new(404))
        .with_priority(1)
        .expect(1)
        .mount(&gmail.server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/messages/send")))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"id": "m9", "threadId": "t9"})),
        )
        .with_priority(2)
        .expect(1)
        .mount(&gmail.server)
        .await;
    let receipt = gmail
        .provider()
        .send(&mail(Some("t-gone"), None))
        .await
        .unwrap();
    assert_eq!(
        receipt,
        SentReceipt {
            message_id: Some("m9".into()),
            thread_id: Some("t9".into())
        }
    );
    let sent = bodies(&gmail, "POST", "messages/send").await;
    assert_eq!(sent.len(), 2);
    assert_eq!(keys(&sent[0]), ["raw", "threadId"]);
    assert_eq!(
        keys(&sent[1]),
        ["raw"],
        "the second goes without the thread"
    );
    assert_eq!(sent[0]["raw"], sent[1]["raw"], "the message is built once");
    assert_eq!(headers_of(&sent[0]["raw"]), built());
}

#[tokio::test]
async fn draft_save_creates_updates_by_id_and_creates_again_when_gmail_lost_it() {
    let gmail = Gmail::start().await;
    let answer = |d: &str, m: &str| {
        ResponseTemplate::new(200)
            .set_body_json(json!({"id": d, "message": {"id": m, "threadId": "t1"}}))
    };
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/drafts")))
        .respond_with(answer("r2", "m2"))
        .mount(&gmail.server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{API_PATH}/drafts/r1")))
        .respond_with(answer("r1", "m3"))
        .mount(&gmail.server)
        .await;
    Mock::given(method("PUT"))
        .and(path(format!("{API_PATH}/drafts/r-gone")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&gmail.server)
        .await;
    let provider = gmail.provider();
    let created = provider.draft_save(&mail(Some("t1"), None)).await.unwrap();
    assert_eq!(created, ids("r2", "m2"));
    let updated = provider
        .draft_save(&mail(Some("t1"), Some("r1")))
        .await
        .unwrap();
    assert_eq!(updated, ids("r1", "m3"));
    let again = provider
        .draft_save(&mail(None, Some("r-gone")))
        .await
        .unwrap();
    assert_eq!(
        again,
        ids("r2", "m2"),
        "a draft deleted at Gmail is created again"
    );
    let posted = bodies(&gmail, "POST", "drafts").await;
    assert_eq!(posted.len(), 2);
    assert_eq!(keys(&posted[0]), ["message"]);
    assert_eq!(keys(&posted[0]["message"]), ["raw", "threadId"]);
    assert_eq!(posted[0]["message"]["threadId"], "t1");
    assert_eq!(keys(&posted[1]["message"]), ["raw"]);
    let put = bodies(&gmail, "PUT", "drafts/r1").await;
    assert_eq!(keys(&put[0]), ["id", "message"]);
    assert_eq!(put[0]["id"], "r1");
    assert_eq!(headers_of(&put[0]["message"]["raw"]), built());
}

#[tokio::test]
async fn a_draft_in_a_thread_gmail_no_longer_has_is_saved_on_its_own() {
    let gmail = Gmail::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/drafts")))
        .and(body_partial_json(
            json!({"message": {"threadId": "t-gone"}}),
        ))
        .respond_with(ResponseTemplate::new(404))
        .with_priority(1)
        .expect(1)
        .mount(&gmail.server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/drafts")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": "r1", "message": {"id": "m1", "threadId": "t5"}})),
        )
        .with_priority(2)
        .expect(1)
        .mount(&gmail.server)
        .await;
    let saved = gmail
        .provider()
        .draft_save(&mail(Some("t-gone"), None))
        .await
        .unwrap();
    assert_eq!(saved, ids("r1", "m1"));
}

#[tokio::test]
async fn sent_already_is_the_rfc822msgid_search() {
    let gmail = Gmail::start().await;
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages")))
        .and(query_param("q", "rfc822msgid:<w1@example.com>"))
        .and(query_param("maxResults", "1"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"messages": [{"id": "m1", "threadId": "t1"}]})),
        )
        .up_to_n_times(1)
        .mount(&gmail.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"resultSizeEstimate": 0})))
        .mount(&gmail.server)
        .await;
    let provider = gmail.provider();
    assert!(provider.sent_already(&mail(None, None)).await.unwrap());
    assert!(!provider.sent_already(&mail(None, None)).await.unwrap());
}

#[tokio::test]
async fn gmail_consumes_no_draft_carries_no_originals_and_refuses_an_unread_attachment() {
    let gmail = Gmail::start().await;
    let provider = gmail.provider();
    assert!(!provider.send_consumes_draft());
    assert!(!provider.forward_carries_originals());
    let mut carried = mail(None, None);
    carried.outgoing.attachments.push(OutgoingAttachment {
        name: "a.pdf".into(),
        mime: "application/pdf".into(),
        bytes: vec![],
        carried: Some(Carried {
            message: "m1".into(),
            part: "p1".into(),
            size: 10,
        }),
    });
    let send = provider.send(&carried).await.unwrap_err();
    let save = provider.draft_save(&carried).await.unwrap_err();
    for error in [send, save] {
        assert!(
            matches!(error, ProviderError::Permanent { .. }),
            "{error:?}"
        );
    }
    assert!(gmail.server.received_requests().await.unwrap().is_empty());
}
