//! Returning snoozed mail on Gmail: one `messages.batchModify` adding `INBOX` to exactly the
//! recorded message ids, at most a thousand ids to a call; nothing to return sends nothing; an
//! id Rail would not send is refused before Gmail is called.

use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{API_PATH, Gmail};
use crate::provider::{Change, Provider, ThreadTarget};

fn unsnooze(ids: &[&str]) -> Change {
    Change::Unsnooze {
        messages: ids.iter().map(|id| (*id).to_string()).collect(),
    }
}

#[tokio::test]
async fn unsnooze_adds_inbox_to_exactly_the_recorded_messages_in_one_call() {
    let gmail = Gmail::start().await;
    Mock::given(method("POST"))
        .and(path(format!("{API_PATH}/messages/batchModify")))
        .and(body_json(
            json!({"ids": ["m1", "m2"], "addLabelIds": ["INBOX"]}),
        ))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&gmail.server)
        .await;
    gmail
        .provider()
        .apply(&[ThreadTarget::from("t1")], &unsnooze(&["m1", "m2"]))
        .await
        .unwrap();
}

#[tokio::test]
async fn unsnooze_sends_at_most_a_thousand_ids_to_a_call() {
    let gmail = Gmail::start().await;
    let ids: Vec<String> = (0..1_001).map(|n| format!("m{n}")).collect();
    let (first, rest) = ids.split_at(1_000);
    for chunk in [first, rest] {
        Mock::given(method("POST"))
            .and(path(format!("{API_PATH}/messages/batchModify")))
            .and(body_json(json!({"ids": chunk, "addLabelIds": ["INBOX"]})))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&gmail.server)
            .await;
    }
    let all: Vec<&str> = ids.iter().map(String::as_str).collect();
    gmail
        .provider()
        .apply(&[ThreadTarget::from("t1")], &unsnooze(&all))
        .await
        .unwrap();
}

#[tokio::test]
async fn unsnooze_with_nothing_to_return_sends_nothing() {
    let gmail = Gmail::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&gmail.server)
        .await;
    gmail
        .provider()
        .apply(&[ThreadTarget::from("t1")], &unsnooze(&[]))
        .await
        .unwrap();
}

#[tokio::test]
async fn unsnooze_refuses_an_id_rail_does_not_send_before_calling_gmail() {
    let gmail = Gmail::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(204))
        .expect(0)
        .mount(&gmail.server)
        .await;
    let error = gmail
        .provider()
        .apply(&[ThreadTarget::from("t1")], &unsnooze(&["m1", "../m2"]))
        .await
        .unwrap_err();
    assert_eq!(error.code(), "permanent", "{error:?}");
}
