//! The folder listing against fake Graph: a nested tree named `Parent/Child` with parents,
//! the well-known kinds, search folders and non-mail roots left out with their children,
//! paging, ids Rail would not send, and a failing well-known lookup.

use serde_json::json;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{Graph, ROOT, folder};
use crate::provider::{FolderKind, Provider, ProviderError, RemoteFolder};

fn s(text: &str) -> String {
    text.to_string()
}

fn ids(found: &[RemoteFolder]) -> Vec<&str> {
    found.iter().map(|f| f.remote_id.as_str()).collect()
}

#[tokio::test]
async fn a_nested_tree_is_named_parent_slash_child_with_its_parents_and_kinds() {
    let g = Graph::start().await;
    g.mount_root(json!([
        folder("f-inbox", "Inbox", 1),
        folder("f-sent", "Sent Items", 0),
        folder("f-deleted", "Deleted Items", 0),
        folder("f-projects", "Projects", 1),
    ]))
    .await;
    g.mount_children("f-inbox", json!([folder("f-clients", "Clients", 1)]))
        .await;
    g.mount_children("f-clients", json!([folder("f-acme", "Acme", 0)]))
        .await;
    g.mount_children("f-projects", json!([folder("f-2026", "2026", 0)]))
        .await;
    g.mount_well_known(&[
        ("inbox", "f-inbox"),
        ("sentitems", "f-sent"),
        ("deleteditems", "f-deleted"),
    ])
    .await;
    let found = g.provider().folders().await.unwrap();
    let shown: Vec<(String, FolderKind, Option<String>)> = found
        .iter()
        .map(|f| (f.name.clone(), f.kind, f.parent.clone()))
        .collect();
    assert_eq!(
        shown,
        [
            (s("Deleted Items"), FolderKind::Trash, None),
            (s("Inbox"), FolderKind::Inbox, None),
            (s("Inbox/Clients"), FolderKind::User, Some(s("f-inbox"))),
            (
                s("Inbox/Clients/Acme"),
                FolderKind::User,
                Some(s("f-clients"))
            ),
            (s("Projects"), FolderKind::User, None),
            (s("Projects/2026"), FolderKind::User, Some(s("f-projects"))),
            (s("Sent Items"), FolderKind::Sent, None),
        ]
    );
    let acme = found
        .iter()
        .find(|f| f.name == "Inbox/Clients/Acme")
        .unwrap();
    assert_eq!(acme.remote_id, "f-acme");
}

#[tokio::test]
async fn search_folders_and_non_mail_roots_are_left_out_with_their_children() {
    let g = Graph::start().await;
    let mut search = folder("f-search", "Unread mail", 0);
    search["@odata.type"] = json!("#microsoft.graph.mailSearchFolder");
    g.mount_root(json!([
        folder("f-inbox", "Inbox", 0),
        search,
        folder("f-conv", "Conversation History", 1),
        folder("f-rss", "RSS Feeds", 0),
        folder("f-subs", "RSS Subscriptions", 0),
        folder("f-sync", "Sync Issues", 3),
    ]))
    .await;
    for skipped in ["f-conv", "f-sync"] {
        Mock::given(method("GET"))
            .and(path(format!(
                "{ROOT}/me/mailFolders/{skipped}/childFolders"
            )))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
            .expect(0)
            .mount(&g.server)
            .await;
    }
    g.mount_well_known(&[("inbox", "f-inbox")]).await;
    let found = g.provider().folders().await.unwrap();
    assert_eq!(ids(&found), ["f-inbox"]);
}

#[tokio::test]
async fn every_page_of_a_listing_is_followed() {
    let g = Graph::start().await;
    let first = json!({
        "value": [folder("f-inbox", "Inbox", 0)],
        "@odata.nextLink": g.link_to("folders-2"),
    });
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/mailFolders")))
        .and(query_param("includeHiddenFolders", "true"))
        .and(query_param("$top", "100"))
        .respond_with(ResponseTemplate::new(200).set_body_json(first))
        .expect(1)
        .mount(&g.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/links/folders-2")))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"value": [folder("f-later", "Later", 0)]})),
        )
        .expect(1)
        .mount(&g.server)
        .await;
    g.mount_well_known(&[]).await;
    let found = g.provider().folders().await.unwrap();
    assert_eq!(ids(&found), ["f-inbox", "f-later"]);
}

#[tokio::test]
async fn ids_rail_would_not_put_in_a_path_are_left_out() {
    let g = Graph::start().await;
    g.mount_root(json!([
        folder("f-inbox", "Inbox", 0),
        folder("../me", "Sneaky", 1),
        folder("a b", "Space", 0),
    ]))
    .await;
    g.mount_well_known(&[("inbox", "f-inbox")]).await;
    let found = g.provider().folders().await.unwrap();
    assert_eq!(ids(&found), ["f-inbox"]);
}

#[tokio::test]
async fn a_failing_well_known_lookup_fails_the_listing() {
    let g = Graph::start().await;
    g.mount_root(json!([folder("f-inbox", "Inbox", 0)])).await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/mailFolders/inbox")))
        .respond_with(ResponseTemplate::new(503))
        .mount(&g.server)
        .await;
    let error = g.provider().folders().await.unwrap_err();
    assert!(
        matches!(error, ProviderError::Transient { .. }),
        "{error:?}"
    );
}
