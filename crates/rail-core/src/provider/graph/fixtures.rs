//! Fake Microsoft Graph: JSON in the shapes of the Graph v1.0 reference, with fake data, served
//! by wiremock under `/v1.0/`.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

use super::{GRAPH_CONCURRENCY, GraphProvider};
use crate::provider::http::{ApiClient, Dialect, Quota, RetryPolicy, api_http_client};
use crate::provider::tokens::FixedTokens;

/// Where the fake API lives on the mock server.
pub(crate) const ROOT: &str = "/v1.0";

/// A Graph client for the fake API at `base`: quick retries, no quota, the real limiter.
pub(crate) fn api_at(base: &str) -> ApiClient {
    ApiClient::new(
        api_http_client().unwrap(),
        Url::parse(base).unwrap(),
        FixedTokens::new(["test-access"]),
    )
    .with_policy(RetryPolicy::quick())
    .with_quota(Quota::unlimited())
    .with_dialect(Dialect::Graph)
    .with_concurrency(GRAPH_CONCURRENCY)
}

pub(crate) struct Graph {
    pub server: MockServer,
    pub batch: FakeBatch,
}

impl Graph {
    pub(crate) async fn start() -> Self {
        let server = MockServer::start().await;
        let batch = FakeBatch::default();
        Mock::given(method("POST"))
            .and(path(format!("{ROOT}/$batch")))
            .respond_with(batch.clone())
            .mount(&server)
            .await;
        Self { server, batch }
    }

    pub(crate) fn base(&self) -> String {
        format!("{}{ROOT}/", self.server.uri())
    }

    pub(crate) fn provider(&self) -> GraphProvider {
        GraphProvider::new(api_at(&self.base()))
    }

    /// An absolute link under the fake API, as Graph writes its next and delta links.
    pub(crate) fn link_to(&self, name: &str) -> String {
        format!("{}{ROOT}/links/{name}", self.server.uri())
    }

    /// Serve `folders` as the root's children (one page).
    pub(crate) async fn mount_root(&self, folders: Value) {
        self.mount_folders("me/mailFolders", folders).await;
    }

    /// Serve `folders` as `parent`'s children (one page).
    pub(crate) async fn mount_children(&self, parent: &str, folders: Value) {
        let at = format!("me/mailFolders/{parent}/childFolders");
        self.mount_folders(&at, folders).await;
    }

    async fn mount_folders(&self, at: &str, folders: Value) {
        Mock::given(method("GET"))
            .and(path(format!("{ROOT}/{at}")))
            .and(query_param("includeHiddenFolders", "true"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": folders})))
            .mount(&self.server)
            .await;
    }

    /// The well-known folders the mailbox has, `(name, id)`; the other names answer 404.
    pub(crate) async fn mount_well_known(&self, known: &[(&str, &str)]) {
        for (name, id) in known {
            Mock::given(method("GET"))
                .and(path(format!("{ROOT}/me/mailFolders/{name}")))
                .and(query_param("$select", "id"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
                .mount(&self.server)
                .await;
        }
    }
}

/// A `mailFolder` as Graph lists it.
pub(crate) fn folder(id: &str, name: &str, children: u32) -> Value {
    json!({
        "id": id, "displayName": name, "parentFolderId": "f-root",
        "childFolderCount": children, "unreadItemCount": 0, "totalItemCount": 0,
        "isHidden": false
    })
}

/// A `message` as the delta and a single get answer it, received (and sent) at `received`.
pub(crate) fn message(id: &str, conversation: &str, folder: &str, received: &str) -> Value {
    json!({
        "@odata.type": "#microsoft.graph.message",
        "id": id, "conversationId": conversation,
        "internetMessageId": format!("<{id}@example.com>"),
        "subject": format!("subject {id}"),
        "from": {"emailAddress": {"name": "Ann", "address": "ann@example.com"}},
        "sender": {"emailAddress": {"name": "Ann", "address": "ann@example.com"}},
        "toRecipients": [{"emailAddress": {"name": "Me", "address": "me@corp.example"}}],
        "ccRecipients": [], "bccRecipients": [], "replyTo": [],
        "receivedDateTime": received, "sentDateTime": received,
        "isRead": false, "isDraft": false, "flag": {"flagStatus": "notFlagged"},
        "bodyPreview": format!("preview {id}"), "parentFolderId": folder,
        "hasAttachments": false
    })
}

/// Answers `$batch` posts from a table keyed by `METHOD /path` (the query left out), in reverse
/// order as Graph may; a request with no answer gets a 404. Records every sub-request.
#[derive(Clone, Default)]
pub(crate) struct FakeBatch {
    answers: Arc<Mutex<HashMap<String, (u16, Value)>>>,
    throttled: Arc<Mutex<HashSet<String>>>,
    seen: Arc<Mutex<Vec<Value>>>,
    posts: Arc<AtomicUsize>,
}

impl FakeBatch {
    pub(crate) fn answer(&self, method: &str, path: &str, status: u16, body: Value) {
        let key = format!("{method} {path}");
        self.answers.lock().unwrap().insert(key, (status, body));
    }

    /// The next batch answers this request with a 429 (a throttled item).
    pub(crate) fn throttle_once(&self, method: &str, path: &str) {
        self.throttled
            .lock()
            .unwrap()
            .insert(format!("{method} {path}"));
    }

    /// Every sub-request posted so far, in order.
    pub(crate) fn requests(&self) -> Vec<Value> {
        self.seen.lock().unwrap().clone()
    }

    /// How many batches were posted.
    pub(crate) fn posts(&self) -> usize {
        self.posts.load(Ordering::SeqCst)
    }

    fn reply(&self, sub: &Value) -> Value {
        let url = sub["url"].as_str().unwrap_or_default();
        let bare = url.split('?').next().unwrap_or_default();
        let key = format!("{} {bare}", sub["method"].as_str().unwrap_or_default());
        let (status, body) = if self.throttled.lock().unwrap().remove(&key) {
            (
                429,
                json!({"error": {"code": "TooManyRequests", "message": "slow down"}}),
            )
        } else {
            self.answers
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .unwrap_or_else(|| {
                    (
                        404,
                        json!({"error": {"code": "ErrorItemNotFound", "message": "not found"}}),
                    )
                })
        };
        json!({"id": sub["id"], "status": status, "headers": {"Retry-After": "1"}, "body": body})
    }
}

impl Respond for FakeBatch {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        self.posts.fetch_add(1, Ordering::SeqCst);
        let posted: Value = serde_json::from_slice(&request.body).unwrap();
        let subs = posted["requests"].as_array().cloned().unwrap_or_default();
        let mut responses = Vec::new();
        for sub in subs {
            self.seen.lock().unwrap().push(sub.clone());
            responses.push(self.reply(&sub));
        }
        responses.reverse();
        ResponseTemplate::new(200).set_body_json(json!({"responses": responses}))
    }
}
