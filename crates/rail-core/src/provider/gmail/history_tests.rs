//! History, server search and the `Provider` trait against fake Gmail.

use std::sync::Arc;

use serde_json::json;
use wiremock::matchers::{path, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{API_PATH, Gmail, history_json, labels_json, list_json, message_json};
use super::*;
use crate::provider::{Provider, Relabel};

fn added(id: &str) -> serde_json::Value {
    json!({"message": {"id": id, "threadId": format!("t-{id}"), "labelIds": ["INBOX"]}})
}

fn labelled(id: &str, labels: &[&str]) -> serde_json::Value {
    json!({"message": {"id": id, "threadId": "t"}, "labelIds": labels})
}

#[tokio::test]
async fn history_gives_adds_deletes_and_label_changes() {
    let gmail = Gmail::start().await;
    let records = json!([
        {"id": "101", "messagesAdded": [added("new1"), added("brief")]},
        {"id": "102", "messagesDeleted": [added("brief"), added("old9")]},
        {"id": "103", "labelsRemoved": [labelled("m5", &["UNREAD", "INBOX"])]},
        {"id": "104", "labelsAdded": [labelled("m5", &["STARRED", "Label_12"]), labelled("new1", &["IMPORTANT"])]}
    ]);
    Mock::given(path(format!("{API_PATH}/history")))
        .and(query_param("startHistoryId", "100"))
        .and(query_param("maxResults", "500"))
        .respond_with(ResponseTemplate::new(200).set_body_json(history_json(records, None, "120")))
        .expect(1)
        .mount(&gmail.server)
        .await;
    gmail
        .mount_message(message_json(
            "new1",
            "t-new1",
            5_000,
            &["INBOX", "IMPORTANT"],
            &[],
        ))
        .await;
    let set = gmail
        .provider()
        .history(&SyncCursor("100".into()))
        .await
        .unwrap();
    let added: Vec<&str> = set.added.iter().map(|e| e.remote_id.as_str()).collect();
    assert_eq!(
        added,
        ["new1"],
        "brief was added and deleted in the same window"
    );
    let mut deleted = set.deleted.clone();
    deleted.sort();
    assert_eq!(deleted, ["brief", "old9"]);
    assert_eq!(
        set.relabeled,
        [Relabel {
            remote_id: "m5".into(),
            add_folders: vec!["Label_12".into()],
            remove_folders: vec!["INBOX".into()],
            read: Some(true),
            starred: Some(true),
        }],
        "new1's label change is already in its fresh metadata"
    );
    assert_eq!(set.cursor, SyncCursor("120".into()));
    assert!(!set.more);
    let types: Vec<String> = gmail.server.received_requests().await.unwrap()[0]
        .url
        .query_pairs()
        .filter(|(k, _)| k == "historyTypes")
        .map(|(_, v)| v.into_owned())
        .collect();
    assert_eq!(
        types,
        [
            "messageAdded",
            "messageDeleted",
            "labelAdded",
            "labelRemoved"
        ]
    );
}

#[tokio::test]
async fn an_expired_history_id_is_cursor_expired() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/history")))
        .respond_with(ResponseTemplate::new(404).set_body_json(json!({
            "error": {"code": 404, "message": "Requested entity was not found.", "errors": [{"reason": "notFound"}]}
        })))
        .mount(&gmail.server)
        .await;
    let provider = gmail.provider();
    let err = provider.history(&SyncCursor("5".into())).await.unwrap_err();
    assert_eq!(err, ProviderError::CursorExpired);
    let odd = provider
        .history(&SyncCursor("not-a-number".into()))
        .await
        .unwrap_err();
    assert_eq!(
        odd,
        ProviderError::CursorExpired,
        "a cursor Rail cannot use is resynced"
    );
    let long = provider
        .history(&SyncCursor("1".repeat(21)))
        .await
        .unwrap_err();
    assert_eq!(long, ProviderError::CursorExpired, "more digits than a u64");
}

#[tokio::test]
async fn history_reads_at_most_ten_pages_and_says_there_is_more() {
    let gmail = Gmail::start().await;
    let records = json!([{"id": "200", "messagesDeleted": [added("x")]}]);
    Mock::given(path(format!("{API_PATH}/history")))
        .respond_with(ResponseTemplate::new(200).set_body_json(history_json(
            records,
            Some("again"),
            "999",
        )))
        .expect(10)
        .mount(&gmail.server)
        .await;
    let set = gmail
        .provider()
        .history(&SyncCursor("100".into()))
        .await
        .unwrap();
    assert!(set.more);
    assert_eq!(
        set.cursor,
        SyncCursor("200".into()),
        "resume after the last record read"
    );
}

#[tokio::test]
async fn empty_history_pages_with_tokens_still_end_and_move_the_cursor() {
    let gmail = Gmail::start().await;
    // Three empty pages that each promise another, then the last page.
    for (from, to) in [(None, "p1"), (Some("p1"), "p2"), (Some("p2"), "p3")] {
        let mock = Mock::given(path(format!("{API_PATH}/history")));
        let mock = match from {
            Some(token) => mock.and(query_param("pageToken", token)),
            None => mock.and(query_param_is_missing("pageToken")),
        };
        mock.respond_with(ResponseTemplate::new(200).set_body_json(history_json(
            json!([]),
            Some(to),
            "150",
        )))
        .expect(1)
        .mount(&gmail.server)
        .await;
    }
    Mock::given(path(format!("{API_PATH}/history")))
        .and(query_param("pageToken", "p3"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"historyId": "150"})))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let set = gmail
        .provider()
        .history(&SyncCursor("100".into()))
        .await
        .unwrap();
    assert_eq!((set.cursor, set.more), (SyncCursor("150".into()), false));
}

#[tokio::test]
async fn endless_empty_history_pages_never_say_more_with_the_same_cursor() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/history")))
        .respond_with(ResponseTemplate::new(200).set_body_json(history_json(
            json!([]),
            Some("again"),
            "150",
        )))
        .expect(MAX_HISTORY_PAGES as u64)
        .mount(&gmail.server)
        .await;
    let set = gmail
        .provider()
        .history(&SyncCursor("100".into()))
        .await
        .unwrap();
    assert!(
        !(set.more && set.cursor == SyncCursor("100".into())),
        "{set:?}"
    );
    assert_eq!(set.cursor, SyncCursor("150".into()));
    assert!(!set.more);
}

#[tokio::test]
async fn unsafe_ids_in_history_records_are_dropped() {
    let gmail = Gmail::start().await;
    let records = json!([
        {"id": "101", "messagesAdded": [added("../profile"), added("a b")]},
        {"id": "102", "messagesDeleted": [added("gone/1"), added("ok1")]},
        {"id": "103", "labelsAdded": [labelled("m5", &["Label_1", "../../x", "Label 2"]), labelled("m?6", &["INBOX"])]},
        {"id": "not-a-number", "labelsRemoved": [labelled("m5", &["INBOX"])]}
    ]);
    Mock::given(path(format!("{API_PATH}/history")))
        .respond_with(ResponseTemplate::new(200).set_body_json(history_json(records, None, "x9")))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let set = gmail
        .provider()
        .history(&SyncCursor("100".into()))
        .await
        .unwrap();
    assert!(set.added.is_empty() && set.skipped.is_empty(), "{set:?}");
    assert_eq!(set.deleted, ["ok1"]);
    assert_eq!(
        set.relabeled,
        [Relabel {
            remote_id: "m5".into(),
            add_folders: vec!["Label_1".into()],
            remove_folders: vec!["INBOX".into()],
            ..Relabel::default()
        }]
    );
    assert_eq!(
        set.cursor,
        SyncCursor("103".into()),
        "only a numeric history id becomes the cursor"
    );
    let requests = gmail.server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1, "no unsafe id reached a request path");
}

#[tokio::test]
async fn a_page_with_too_many_records_resumes_after_the_last_one_kept() {
    let gmail = Gmail::start().await;
    let records: Vec<serde_json::Value> = (0..MAX_HISTORY_RECORDS + 5)
        .map(
            |i| json!({"id": (1000 + i).to_string(), "messagesDeleted": [added(&format!("d{i}"))]}),
        )
        .collect();
    Mock::given(path(format!("{API_PATH}/history")))
        .respond_with(ResponseTemplate::new(200).set_body_json(history_json(
            json!(records),
            None,
            "9999",
        )))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let set = gmail
        .provider()
        .history(&SyncCursor("100".into()))
        .await
        .unwrap();
    assert_eq!(set.deleted.len(), MAX_HISTORY_RECORDS);
    assert!(set.more);
    assert_eq!(
        set.cursor,
        SyncCursor((1000 + MAX_HISTORY_RECORDS - 1).to_string())
    );
}

#[tokio::test]
async fn search_sends_the_users_words_as_q_without_spam_and_trash() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/messages")))
        .and(query_param("q", "from:dana \"budget 2024\""))
        .and(query_param("includeSpamTrash", "false"))
        .and(query_param("maxResults", "5"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(list_json(&["a", "b"], Some("more"))),
        )
        .expect(1)
        .mount(&gmail.server)
        .await;
    let ids = gmail
        .provider()
        .search_ids("from:dana \"budget 2024\"", 5)
        .await
        .unwrap();
    assert_eq!(ids, ["a", "b"]);
}

#[tokio::test]
async fn search_asks_for_at_most_the_cap_and_drops_unsafe_ids() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/messages")))
        .and(query_param("maxResults", MAX_SEARCH_RESULTS.to_string()))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(list_json(&["a", "../b", "c"], None)),
        )
        .expect(1)
        .mount(&gmail.server)
        .await;
    let ids = gmail.provider().search_ids("x", 10_000).await.unwrap();
    assert_eq!(ids, ["a", "c"]);
}

#[tokio::test]
async fn the_provider_works_as_a_trait_object() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/labels")))
        .respond_with(ResponseTemplate::new(200).set_body_json(labels_json()))
        .mount(&gmail.server)
        .await;
    let provider: Arc<dyn Provider> = Arc::new(gmail.provider());
    assert!(!provider.folders().await.unwrap().is_empty());
}
