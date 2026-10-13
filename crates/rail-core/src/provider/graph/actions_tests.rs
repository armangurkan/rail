//! Thread actions against fake Graph: the move, flag and read bodies; only the messages the
//! scope rule picks are sent, to the right destination, twenty to a batch, each asking for
//! immutable ids; nothing to touch sends nothing; a gone message is skipped and one refusal
//! among several still succeeds, while every message refused fails with Graph's first reason;
//! a 401 item needs a login and a transient one retries the row; a missing Archive and a move
//! into a folder gone send nothing; labels are refused; refusals are logged by id only.

use serde_json::{Value, json};
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::MOVE_TARGET_GONE;
use super::errors::CONSENT_MESSAGE;
use super::fixtures::{Graph, ROOT, folder};
use crate::ops::{M365_LABELS, M365_NO_ARCHIVE};
use crate::provider::http::IMMUTABLE_IDS;
use crate::provider::{Change, FolderKind, Provider, ProviderError, TargetMessage, ThreadTarget};

fn msg(
    id: &str,
    kind: FolderKind,
    folder: &str,
    unread: bool,
    starred: bool,
    date: i64,
) -> TargetMessage {
    TargetMessage {
        id: id.into(),
        folder: Some(kind),
        folder_remote: Some(folder.into()),
        unread,
        starred,
        date,
    }
}

fn target(thread: &str, messages: Vec<TargetMessage>) -> ThreadTarget {
    ThreadTarget {
        thread_id: thread.into(),
        messages,
    }
}

/// Conversation c1: an unread Inbox message, the user's reply in Sent Items, and a flagged
/// one filed in Projects.
fn spread() -> ThreadTarget {
    target(
        "c1",
        vec![
            msg("m-in", FolderKind::Inbox, "f-inbox", true, false, 10),
            msg("m-sent", FolderKind::Sent, "f-sent", false, false, 20),
            msg("m-proj", FolderKind::User, "f-proj", false, true, 15),
        ],
    )
}

/// Inbox messages `ids`, oldest first, each `unread`.
fn inbox(ids: &[&str], unread: bool) -> ThreadTarget {
    let messages = ids
        .iter()
        .zip(1..)
        .map(|(id, date)| msg(id, FolderKind::Inbox, "f-inbox", unread, false, date))
        .collect();
    target("c1", messages)
}

/// Inbox, Sent Items, Archive, Deleted Items and Projects; the first four well-known.
async fn mailbox(g: &Graph) {
    g.mount_root(json!([
        folder("f-inbox", "Inbox", 0),
        folder("f-sent", "Sent Items", 0),
        folder("f-arch", "Archive", 0),
        folder("f-del", "Deleted Items", 0),
        folder("f-proj", "Projects", 0),
    ]))
    .await;
    g.mount_well_known(&[
        ("inbox", "f-inbox"),
        ("sentitems", "f-sent"),
        ("archive", "f-arch"),
        ("deleteditems", "f-del"),
    ])
    .await;
}

fn urls(g: &Graph) -> Vec<String> {
    g.batch
        .requests()
        .iter()
        .map(|r| r["url"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn archive_trash_and_move_post_one_move_per_touched_message_to_the_right_folder() {
    let to = |id: &str| Change::Move { to: id.into() };
    let cases = [
        (Change::Archive, vec!["m-in"], "f-arch"),
        (Change::Trash, vec!["m-in", "m-proj"], "f-del"),
        (to("f-proj"), vec!["m-in"], "f-proj"),
        (to("f-inbox"), vec!["m-proj"], "f-inbox"),
    ];
    for (change, moved, destination) in cases {
        let g = Graph::start().await;
        mailbox(&g).await;
        for id in &moved {
            let at = format!("/me/messages/{id}/move");
            g.batch.answer("POST", &at, 201, json!({"id": id}));
        }
        g.provider().apply(&[spread()], &change).await.unwrap();
        let wanted: Vec<String> = moved
            .iter()
            .map(|id| format!("/me/messages/{id}/move"))
            .collect();
        assert_eq!(
            urls(&g),
            wanted,
            "{change:?}: the reply in Sent Items never moves"
        );
        for request in g.batch.requests() {
            assert_eq!(request["method"], "POST");
            assert_eq!(request["body"], json!({"destinationId": destination}));
            assert_eq!(request["headers"]["Prefer"], IMMUTABLE_IDS);
            assert_eq!(request["headers"]["Content-Type"], "application/json");
        }
    }
}

#[tokio::test]
async fn star_and_read_patch_only_the_messages_the_scope_rule_picks() {
    let flag = |status: &str| json!({"flag": {"flagStatus": status}});
    let mut both = inbox(&["m-old", "m-new"], false);
    both.messages[0].starred = true;
    both.messages[1].starred = true;
    let mut fresh = inbox(&["m-old", "m-new"], false);
    fresh.messages[1].unread = true;
    type Case<'a> = (ThreadTarget, Change, Vec<(&'a str, Value)>);
    let cases: Vec<Case> = vec![
        (
            fresh,
            Change::Read { on: true },
            vec![("m-new", json!({"isRead": true}))],
        ),
        (
            inbox(&["m-old", "m-new"], false),
            Change::Read { on: false },
            vec![("m-new", json!({"isRead": false}))],
        ),
        (
            inbox(&["m-old", "m-new"], false),
            Change::Star { on: true },
            vec![("m-new", flag("flagged"))],
        ),
        (
            both,
            Change::Star { on: false },
            vec![("m-old", flag("notFlagged")), ("m-new", flag("notFlagged"))],
        ),
    ];
    for (thread, change, wanted) in cases {
        let g = Graph::start().await;
        g.provider().apply(&[thread], &change).await.unwrap();
        let sent: Vec<(String, Value)> = g
            .batch
            .requests()
            .iter()
            .map(|r| {
                assert_eq!(r["method"], "PATCH", "{change:?}");
                assert_eq!(r["headers"]["Prefer"], IMMUTABLE_IDS);
                (
                    r["url"].as_str().unwrap_or_default().to_string(),
                    r["body"].clone(),
                )
            })
            .collect();
        let wanted: Vec<(String, Value)> = wanted
            .into_iter()
            .map(|(id, body)| (format!("/me/messages/{id}"), body))
            .collect();
        assert_eq!(sent, wanted, "{change:?}");
    }
}

#[tokio::test]
async fn mark_read_patches_twenty_to_a_batch() {
    let g = Graph::start().await;
    let many: Vec<String> = (0..25).map(|n| format!("m{n}")).collect();
    let ids: Vec<&str> = many.iter().map(String::as_str).collect();
    for id in &many {
        let at = format!("/me/messages/{id}");
        g.batch
            .answer("PATCH", &at, 200, json!({"id": id, "isRead": true}));
    }
    let threads = [inbox(&ids[..5], true), inbox(&ids[5..], true)];
    g.provider()
        .apply(&threads, &Change::Read { on: true })
        .await
        .unwrap();
    assert_eq!(g.batch.posts(), 2);
    assert_eq!(g.batch.requests().len(), 25);
}

#[tokio::test]
async fn nothing_to_touch_sends_nothing() {
    let g = Graph::start().await;
    let provider = g.provider();
    let read = inbox(&["m1", "m2"], false);
    provider
        .apply(&[read], &Change::Read { on: true })
        .await
        .unwrap();
    let filed = target(
        "c2",
        vec![msg("m-p", FolderKind::User, "f-proj", false, false, 1)],
    );
    let to = Change::Move {
        to: "f-proj".into(),
    };
    provider.apply(&[filed], &to).await.unwrap();
    provider
        .apply(&[ThreadTarget::from("c-empty")], &Change::Archive)
        .await
        .unwrap();
    assert_eq!(g.batch.posts(), 0, "no batch and no folder listing");
    assert!(g.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_gone_message_and_one_refusal_among_several_still_succeed() {
    let g = Graph::start().await;
    // m1 has no answer: the fake batch answers 404, which is gone and skipped.
    let refused = json!({"error": {"code": "ErrorInvalidRequest", "message": "no"}});
    g.batch.answer("PATCH", "/me/messages/m2", 400, refused);
    g.batch
        .answer("PATCH", "/me/messages/m3", 200, json!({"id": "m3"}));
    g.provider()
        .apply(
            &[inbox(&["m1", "m2", "m3"], true)],
            &Change::Read { on: true },
        )
        .await
        .unwrap();
    assert_eq!(g.batch.requests().len(), 3);
}

#[tokio::test]
async fn every_message_refused_fails_with_graphs_reason_for_the_first() {
    let g = Graph::start().await;
    for id in ["m1", "m2"] {
        let refused =
            json!({"error": {"code": "ErrorAccessDenied", "message": format!("no {id}")}});
        g.batch
            .answer("PATCH", &format!("/me/messages/{id}"), 403, refused);
    }
    let error = g
        .provider()
        .apply(&[inbox(&["m1", "m2"], true)], &Change::Read { on: true })
        .await
        .unwrap_err();
    assert_eq!(error.to_string(), CONSENT_MESSAGE);
    assert_eq!(error.detail(), Some("ErrorAccessDenied: no m1"));
}

#[tokio::test]
async fn a_401_item_needs_a_login_and_a_503_item_retries_the_row() {
    let g = Graph::start().await;
    let expired = json!({"error": {"code": "InvalidAuthenticationToken", "message": "expired"}});
    g.batch.answer("PATCH", "/me/messages/m1", 401, expired);
    g.batch
        .answer("PATCH", "/me/messages/m2", 200, json!({"id": "m2"}));
    let error = g
        .provider()
        .apply(&[inbox(&["m1", "m2"], true)], &Change::Read { on: true })
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::NeedsAuth { .. }),
        "{error:?}"
    );
    let g = Graph::start().await;
    let busy = json!({"error": {"code": "ServiceUnavailable", "message": "later"}});
    g.batch.answer("PATCH", "/me/messages/m1", 503, busy);
    let error = g
        .provider()
        .apply(&[inbox(&["m1"], true)], &Change::Read { on: true })
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Transient { .. }),
        "{error:?}"
    );
}

#[tokio::test]
async fn a_throttled_item_is_sent_again_alone() {
    let g = Graph::start().await;
    g.batch.throttle_once("PATCH", "/me/messages/m2");
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/m2")))
        .and(body_json(json!({"isRead": true})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": "m2", "isRead": true})))
        .expect(1)
        .mount(&g.server)
        .await;
    g.provider()
        .apply(&[inbox(&["m1", "m2"], true)], &Change::Read { on: true })
        .await
        .unwrap();
    assert_eq!(g.batch.posts(), 1);
}

#[tokio::test]
async fn archive_with_no_archive_folder_and_a_move_into_a_folder_gone_send_nothing() {
    let g = Graph::start().await;
    g.mount_root(json!([folder("f-inbox", "Inbox", 0)])).await;
    g.mount_well_known(&[("inbox", "f-inbox")]).await;
    let provider = g.provider();
    let archive = provider.apply(&[spread()], &Change::Archive).await;
    assert_eq!(archive, Err(ProviderError::permanent(M365_NO_ARCHIVE)));
    let to = Change::Move {
        to: "f-proj".into(),
    };
    let gone = provider.apply(&[spread()], &to).await;
    assert_eq!(gone, Err(ProviderError::permanent(MOVE_TARGET_GONE)));
    assert_eq!(g.batch.posts(), 0);
}

#[tokio::test]
async fn labels_are_refused_with_the_folder_text() {
    let g = Graph::start().await;
    let labels = Change::Labels {
        add: vec!["f-proj".into()],
        remove: vec![],
    };
    let error = g.provider().apply(&[spread()], &labels).await;
    assert_eq!(error, Err(ProviderError::permanent(M365_LABELS)));
    assert_eq!(g.batch.posts(), 0);
}

/// `logged` runs its own runtime, so this is a plain `#[test]`.
#[test]
fn a_refused_message_is_logged_by_id_and_status_without_graphs_text() {
    let (result, log) = crate::test_support::logged(async {
        let g = Graph::start().await;
        let refused =
            json!({"error": {"code": "ErrorInvalidRequest", "message": "subject secret"}});
        g.batch.answer("PATCH", "/me/messages/m2", 400, refused);
        g.batch
            .answer("PATCH", "/me/messages/m1", 200, json!({"id": "m1"}));
        g.provider()
            .apply(&[inbox(&["m1", "m2"], true)], &Change::Read { on: true })
            .await
    });
    result.unwrap();
    assert!(log.contains("m2") && log.contains("400"), "{log}");
    assert!(!log.contains("subject secret"), "{log}");
}

#[tokio::test]
async fn a_410_on_a_move_is_a_refusal_not_an_expired_cursor() {
    let g = Graph::start().await;
    mailbox(&g).await;
    let gone = json!({"error": {"code": "Gone", "message": "gone"}});
    g.batch.answer("POST", "/me/messages/m-in/move", 410, gone);
    let error = g
        .provider()
        .apply(&[spread()], &Change::Archive)
        .await
        .unwrap_err();
    assert!(
        matches!(&error, ProviderError::Permanent { message, .. } if message == "Microsoft 365 refused the request (HTTP 410)"),
        "{error:?}"
    );
}
