//! The Gmail provider against fake Gmail.

use std::time::{Duration, Instant};

use serde_json::json;
use wiremock::matchers::{method, path, query_param, query_param_is_missing};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{API_PATH, Gmail, labels_json, list_json, message_json};
use super::metadata::{MAX_ADDRESSES, MAX_SUBJECT_CHARS, unescape_snippet};
use super::*;
use crate::provider::{FolderKind, Mailbox, Window};

const DAY_MS: i64 = 86_400_000;

#[tokio::test]
async fn labels_map_system_labels_to_kinds_and_flags_to_nothing() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/labels")))
        .respond_with(ResponseTemplate::new(200).set_body_json(labels_json()))
        .mount(&gmail.server)
        .await;
    let folders = gmail.provider().labels().await.unwrap();
    let got: Vec<(&str, &str, FolderKind)> = folders
        .iter()
        .map(|f| (f.remote_id.as_str(), f.name.as_str(), f.kind))
        .collect();
    assert_eq!(
        got,
        [
            ("INBOX", "Inbox", FolderKind::Inbox),
            ("SENT", "Sent", FolderKind::Sent),
            ("DRAFT", "Drafts", FolderKind::Drafts),
            ("TRASH", "Trash", FolderKind::Trash),
            ("SPAM", "Spam", FolderKind::Spam),
            ("IMPORTANT", "Important", FolderKind::Label),
            ("CATEGORY_SOCIAL", "Category: Social", FolderKind::Label),
            ("Label_12", "Receipts/2026", FolderKind::User),
        ]
    );
}

#[tokio::test]
async fn backfill_pages_newest_first_with_the_window_query() {
    let gmail = Gmail::start().await;
    let window = Window {
        after_ms: Some(1_700_000_000_123),
        before_ms: Some(1_700_100_000_999),
    };
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages")))
        .and(query_param("q", "after:1700000000 before:1700100000"))
        .and(query_param("includeSpamTrash", "true"))
        .and(query_param("maxResults", "100"))
        .and(query_param_is_missing("pageToken"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(list_json(&["m2", "m1"], Some("p2"))),
        )
        .expect(1)
        .mount(&gmail.server)
        .await;
    Mock::given(method("GET"))
        .and(path(format!("{API_PATH}/messages")))
        .and(query_param("pageToken", "p2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list_json(&[], None)))
        .expect(1)
        .mount(&gmail.server)
        .await;
    for (id, ms) in [("m2", 2 * DAY_MS), ("m1", DAY_MS)] {
        gmail
            .mount_message(message_json(
                id,
                "t1",
                ms,
                &["INBOX", "UNREAD"],
                &[("Subject", id)],
            ))
            .await;
    }
    let provider = gmail.provider();
    let first = provider.backfill_page(&window, None).await.unwrap();
    let ids: Vec<&str> = first
        .envelopes
        .iter()
        .map(|e| e.remote_id.as_str())
        .collect();
    assert_eq!(ids, ["m2", "m1"]);
    assert_eq!(first.next, Some(PageToken("p2".into())));
    let second = provider
        .backfill_page(&window, first.next.as_ref())
        .await
        .unwrap();
    assert!(second.envelopes.is_empty() && second.next.is_none());
}

#[tokio::test]
async fn the_whole_mailbox_is_listed_without_a_query() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/messages")))
        .and(query_param_is_missing("q"))
        .respond_with(ResponseTemplate::new(200).set_body_json(list_json(&[], None)))
        .expect(1)
        .mount(&gmail.server)
        .await;
    let page = gmail
        .provider()
        .backfill_page(&Window::default(), None)
        .await
        .unwrap();
    assert!(page.envelopes.is_empty());
}

#[tokio::test]
async fn metadata_decodes_encoded_words_addresses_flags_and_references() {
    let gmail = Gmail::start().await;
    gmail
        .mount_message(message_json(
            "m1",
            "t1",
            1_700_000_000_000,
            &["INBOX", "STARRED", "Label_12"],
            &[
                ("Subject", "=?UTF-8?B?w6l0w6kgcmVwb3J0?="),
                ("From", "=?ISO-8859-1?Q?Andr=E9?= <andre@example.com>"),
                ("To", "a@example.com, \"Bee, B.\" <b@example.com>"),
                ("Cc", "Team: c@example.com, d@example.com;"),
                ("Date", "Tue, 14 Nov 2023 22:13:20 +0000"),
                ("Message-ID", "<m1@example.com>"),
                ("In-Reply-To", "<m0@example.com>"),
                ("References", "<a@example.com> <m0@example.com>"),
            ],
        ))
        .await;
    let page = gmail
        .provider()
        .metadata(&["m1".to_string()])
        .await
        .unwrap();
    let env = &page.envelopes[0];
    assert_eq!(env.subject, "été report");
    assert_eq!(
        env.from,
        [Mailbox {
            name: Some("André".into()),
            address: "andre@example.com".into()
        }]
    );
    let to: Vec<&str> = env.to.iter().map(|m| m.address.as_str()).collect();
    assert_eq!(to, ["a@example.com", "b@example.com"]);
    assert_eq!(env.to[1].name.as_deref(), Some("Bee, B."));
    assert_eq!(env.cc.len(), 2, "group members are flattened");
    assert_eq!(env.date, 1_700_000_000);
    assert_eq!(env.internal_date, 1_700_000_000_000);
    assert_eq!(env.message_id.as_deref(), Some("m1@example.com"));
    assert_eq!(env.in_reply_to.as_deref(), Some("m0@example.com"));
    assert_eq!(env.references, ["a@example.com", "m0@example.com"]);
    assert_eq!(env.folders, ["INBOX", "Label_12"]);
    assert!(env.is_read && env.is_starred && !env.is_draft);
    assert_eq!(env.snippet, "snippet of m1 'quoted' & more");
    assert_eq!(env.size, 2048);
}

#[tokio::test]
async fn missing_headers_and_odd_dates_fall_back() {
    let gmail = Gmail::start().await;
    let internal = 1_700_000_000_000;
    gmail
        .mount_message(message_json("bare", "t", internal, &[], &[]))
        .await;
    gmail
        .mount_message(message_json(
            "junk",
            "t",
            internal,
            &["UNREAD"],
            &[("Date", "yesterday-ish")],
        ))
        .await;
    gmail
        .mount_message(message_json(
            "future",
            "t",
            internal,
            &[],
            &[("Date", "Mon, 1 Jan 2999 00:00:00 +0000")],
        ))
        .await;
    let ids = ["bare", "junk", "future"].map(String::from);
    let page = gmail.provider().metadata(&ids).await.unwrap();
    assert_eq!(page.envelopes.len(), 3);
    for env in &page.envelopes {
        assert_eq!(env.date, internal / 1000, "{}", env.remote_id);
    }
    let bare = &page.envelopes[0];
    assert!(bare.subject.is_empty() && bare.from.is_empty() && bare.message_id.is_none());
    assert!(!page.envelopes[1].is_read);
}

/// Review focus 2.
#[tokio::test]
async fn hostile_headers_are_capped_and_cannot_inject_fields() {
    let gmail = Gmail::start().await;
    let huge = "x".repeat(1_000_000);
    let many: Vec<String> = (0..5000).map(|i| format!("u{i}@example.com")).collect();
    let many = many.join(", ");
    gmail
        .mount_message(message_json(
            "evil",
            "t",
            1_700_000_000_000,
            &["INBOX"],
            &[
                ("Subject", "hello\r\nBcc: victim@example.com\r\n\r\nbody?"),
                ("To", &many),
                ("Cc", "=?UTF-8?B?!!!not-base64?= <c@example.com>"),
                ("References", &huge),
            ],
        ))
        .await;
    gmail
        .mount_message(message_json(
            "long",
            "t",
            1_700_000_000_000,
            &[],
            &[("Subject", &huge)],
        ))
        .await;
    let ids = ["evil", "long"].map(String::from);
    let page = gmail.provider().metadata(&ids).await.unwrap();
    assert!(page.skipped.is_empty(), "{:?}", page.skipped);
    let evil = &page.envelopes[0];
    assert!(evil.bcc.is_empty(), "a value cannot start another header");
    assert!(!evil.subject.contains('\n') && !evil.subject.contains('\r'));
    assert!(evil.subject.starts_with("hello"));
    assert_eq!(evil.to.len(), MAX_ADDRESSES);
    assert_eq!(evil.cc.len(), 1);
    assert!(evil.references.iter().map(String::len).sum::<usize>() <= 16 * 1024);
    assert!(page.envelopes[1].subject.chars().count() <= MAX_SUBJECT_CHARS);
}

#[tokio::test]
async fn a_message_rail_cannot_read_is_skipped_by_id_and_a_vanished_one_dropped() {
    let gmail = Gmail::start().await;
    gmail
        .mount_message(message_json("fine", "t", 1_000, &[], &[]))
        .await;
    let mut no_date = message_json("baddate", "t", 1_000, &[], &[]);
    no_date["internalDate"] = json!("soon");
    gmail.mount_message(no_date).await;
    let mut no_thread = message_json("nothread", "t", 1_000, &[], &[]);
    no_thread.as_object_mut().unwrap().remove("threadId");
    gmail.mount_message(no_thread).await;
    Mock::given(path(format!("{API_PATH}/messages/gone")))
        .respond_with(ResponseTemplate::new(404))
        .mount(&gmail.server)
        .await;
    let ids = ["fine", "baddate", "nothread", "gone", "../labels"].map(String::from);
    let page = gmail.provider().metadata(&ids).await.unwrap();
    let ids: Vec<&str> = page
        .envelopes
        .iter()
        .map(|e| e.remote_id.as_str())
        .collect();
    assert_eq!(ids, ["fine"]);
    assert_eq!(page.skipped, ["baddate", "nothread", "../labels"]);
    let paths: Vec<String> = gmail
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .map(|r| r.url.path().to_string())
        .collect();
    assert!(paths.iter().all(|p| !p.ends_with("/labels")), "{paths:?}");
}

#[tokio::test]
async fn metadata_requests_run_at_most_eight_at_a_time() {
    let gmail = Gmail::start().await;
    let ids: Vec<String> = (0..16).map(|i| format!("m{i}")).collect();
    for id in &ids {
        let json = message_json(id, "t", 1_000, &[], &[]);
        Mock::given(path(format!("{API_PATH}/messages/{id}")))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json)
                    .set_delay(Duration::from_millis(100)),
            )
            .mount(&gmail.server)
            .await;
    }
    let started = Instant::now();
    let page = gmail.provider().metadata(&ids).await.unwrap();
    let took = started.elapsed();
    assert_eq!(page.envelopes.len(), 16);
    assert!(
        took >= Duration::from_millis(200),
        "16 at 8 at a time is two rounds: {took:?}"
    );
    assert!(
        took < Duration::from_millis(1500),
        "they ran concurrently: {took:?}"
    );
}

#[tokio::test]
async fn the_profile_gives_the_current_history_id() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/profile")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "emailAddress": "me@example.com", "messagesTotal": 1234, "threadsTotal": 900, "historyId": "98765"
        })))
        .mount(&gmail.server)
        .await;
    let profile = gmail.provider().read_profile().await.unwrap();
    assert_eq!(profile.cursor, SyncCursor("98765".into()));
    assert_eq!(profile.messages_total, 1234);
}

#[test]
fn snippet_character_references_are_decoded() {
    assert_eq!(
        unescape_snippet("it&#39;s &lt;b&gt; &amp;&quot; &#x263A; &bogus; & x"),
        "it's <b> &\" ☺ &bogus; & x"
    );
}

#[test]
fn window_queries_use_whole_seconds() {
    assert_eq!(list::window_query(&Window::default()), None);
    let after = Window {
        after_ms: Some(5_999),
        before_ms: None,
    };
    assert_eq!(list::window_query(&after).as_deref(), Some("after:5"));
}

/// T3-C(a), spec §5: one answer Rail cannot read is skipped, not the end of the pass.
#[tokio::test]
async fn an_answer_over_the_cap_or_unparseable_is_skipped_by_id() {
    let gmail = Gmail::start().await;
    gmail
        .mount_message(message_json("fine", "t", 1_000, &[], &[]))
        .await;
    let mut big = message_json("big", "t", 1_000, &[], &[]);
    big["snippet"] = json!("y".repeat(64 * 1024));
    gmail.mount_message(big).await;
    Mock::given(path(format!("{API_PATH}/messages/garbled")))
        .respond_with(ResponseTemplate::new(200).set_body_string("{not json"))
        .mount(&gmail.server)
        .await;
    Mock::given(path(format!("{API_PATH}/messages/oddshape")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": 5, "labelIds": "x"})))
        .mount(&gmail.server)
        .await;
    let ids = ["fine", "big", "garbled", "oddshape"].map(String::from);
    let page = gmail
        .provider_capped(16 * 1024)
        .metadata(&ids)
        .await
        .unwrap();
    let read: Vec<&str> = page
        .envelopes
        .iter()
        .map(|e| e.remote_id.as_str())
        .collect();
    assert_eq!(read, ["fine"]);
    assert_eq!(page.skipped, ["big", "garbled", "oddshape"]);
}

/// Any other failure still fails the call: a refusal is not a message to skip.
#[tokio::test]
async fn a_refusal_for_one_message_fails_the_call() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/messages/m1")))
        .respond_with(ResponseTemplate::new(400))
        .mount(&gmail.server)
        .await;
    let err = gmail
        .provider()
        .metadata(&["m1".to_string()])
        .await
        .unwrap_err();
    assert_eq!(err.code(), "permanent", "{err:?}");
}

#[tokio::test]
async fn a_profile_without_a_numeric_history_id_is_refused() {
    let gmail = Gmail::start().await;
    Mock::given(path(format!("{API_PATH}/profile")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"historyId": "12a"})))
        .mount(&gmail.server)
        .await;
    let err = gmail.provider().read_profile().await.unwrap_err();
    assert_eq!(err.code(), "permanent");
}

#[test]
fn ids_are_checked_before_they_reach_a_path() {
    for good in ["18c2f0a1b2c3d4e5", "a-b_C9"] {
        assert_eq!(checked_id(good), Some(good));
    }
    let long = "a".repeat(65);
    for bad in ["", "../labels", "a/b", "a?b", "a%2F", "é", long.as_str()] {
        assert_eq!(checked_id(bad), None, "{bad}");
        assert!(unsafe_id(bad), "{bad}");
    }
    assert!(safe_id("abc", 3) && !safe_id("abcd", 3));
}
