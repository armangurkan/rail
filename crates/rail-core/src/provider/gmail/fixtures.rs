//! Fake Gmail: JSON in the shapes of the Gmail REST reference, with fake data, served by
//! wiremock under `/gmail/v1/users/me/`.

use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use super::GmailProvider;
use crate::provider::http::{ApiClient, Quota, RetryPolicy, api_http_client};
use crate::provider::tokens::FixedTokens;

pub(crate) const API_PATH: &str = "/gmail/v1/users/me";

pub(crate) struct Gmail {
    pub server: MockServer,
}

impl Gmail {
    pub(crate) async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
    }

    pub(crate) fn base(&self) -> String {
        format!("{}{API_PATH}/", self.server.uri())
    }

    pub(crate) fn provider(&self) -> GmailProvider {
        GmailProvider::new(self.api())
    }

    /// A provider that reads answers of at most `bytes`.
    pub(crate) fn provider_capped(&self, bytes: usize) -> GmailProvider {
        GmailProvider::new(self.api().with_max_response(bytes))
    }

    fn api(&self) -> ApiClient {
        ApiClient::new(
            api_http_client().unwrap(),
            Url::parse(&self.base()).unwrap(),
            FixedTokens::new(["test-access"]),
        )
        .with_policy(RetryPolicy::quick())
        .with_quota(Quota::unlimited())
    }

    /// Serve `json` (a `message_json`) at `messages/<its id>`.
    pub(crate) async fn mount_message(&self, json: Value) {
        let id = json["id"].as_str().unwrap().to_string();
        Mock::given(method("GET"))
            .and(path(format!("{API_PATH}/messages/{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json))
            .mount(&self.server)
            .await;
    }
}

/// A `format=metadata` message.
pub(crate) fn message_json(
    id: &str,
    thread: &str,
    internal_ms: i64,
    labels: &[&str],
    headers: &[(&str, &str)],
) -> Value {
    let headers: Vec<Value> = headers
        .iter()
        .map(|(name, value)| json!({"name": name, "value": value}))
        .collect();
    json!({
        "id": id,
        "threadId": thread,
        "labelIds": labels,
        "snippet": format!("snippet of {id} &#39;quoted&#39; &amp; more"),
        "historyId": "1000",
        "internalDate": internal_ms.to_string(),
        "sizeEstimate": 2048,
        "payload": {"partId": "", "mimeType": "multipart/alternative", "filename": "", "headers": headers}
    })
}

pub(crate) fn list_json(ids: &[&str], next: Option<&str>) -> Value {
    let messages: Vec<Value> = ids
        .iter()
        .map(|id| json!({"id": id, "threadId": format!("t-{id}")}))
        .collect();
    let mut list = json!({"resultSizeEstimate": ids.len()});
    if !messages.is_empty() {
        list["messages"] = Value::Array(messages);
    }
    if let Some(next) = next {
        list["nextPageToken"] = json!(next);
    }
    list
}

pub(crate) fn labels_json() -> Value {
    json!({"labels": [
        {"id": "INBOX", "name": "INBOX", "type": "system", "messageListVisibility": "hide", "labelListVisibility": "labelShow"},
        {"id": "SENT", "name": "SENT", "type": "system"},
        {"id": "DRAFT", "name": "DRAFT", "type": "system"},
        {"id": "TRASH", "name": "TRASH", "type": "system"},
        {"id": "SPAM", "name": "SPAM", "type": "system"},
        {"id": "UNREAD", "name": "UNREAD", "type": "system"},
        {"id": "STARRED", "name": "STARRED", "type": "system"},
        {"id": "IMPORTANT", "name": "IMPORTANT", "type": "system"},
        {"id": "CATEGORY_SOCIAL", "name": "CATEGORY_SOCIAL", "type": "system"},
        {"id": "Label_12", "name": "Receipts/2026", "type": "user", "color": {"textColor": "#000000", "backgroundColor": "#ffffff"}}
    ]})
}
