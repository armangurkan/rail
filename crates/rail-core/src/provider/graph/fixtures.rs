//! Fake Microsoft Graph: JSON in the shapes of the Graph v1.0 reference, with fake data, served
//! by wiremock under `/v1.0/`.

use serde_json::{Value, json};
use url::Url;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

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
}

impl Graph {
    pub(crate) async fn start() -> Self {
        Self {
            server: MockServer::start().await,
        }
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
