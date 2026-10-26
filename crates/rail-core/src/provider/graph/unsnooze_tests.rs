//! Returning snoozed mail on Microsoft 365: only the recorded messages still in Archive move,
//! each to the Inbox, asking for immutable ids; an older Archive message, the user's reply in
//! Sent Items and a recorded message now in Deleted Items stay where they are; a message gone
//! from the mailbox counts as done.

use serde_json::json;

use super::fixtures::{Graph, folder};
use crate::provider::http::IMMUTABLE_IDS;
use crate::provider::{Change, FolderKind, Provider, TargetMessage, ThreadTarget};

fn msg(id: &str, kind: FolderKind, folder: &str, date: i64) -> TargetMessage {
    TargetMessage {
        id: id.into(),
        folder: Some(kind),
        folder_remote: Some(folder.into()),
        unread: false,
        starred: false,
        date,
    }
}

/// Conversation c1 after a snooze archived m-a1 and m-a2: m-old was in Archive before the
/// snooze, m-sent is the user's reply, and m-del (recorded) was deleted since.
fn snoozed() -> ThreadTarget {
    ThreadTarget {
        thread_id: "c1".into(),
        messages: vec![
            msg("m-old", FolderKind::Archive, "f-arch", 1),
            msg("m-a1", FolderKind::Archive, "f-arch", 2),
            msg("m-sent", FolderKind::Sent, "f-sent", 3),
            msg("m-a2", FolderKind::Archive, "f-arch", 4),
            msg("m-del", FolderKind::Trash, "f-del", 5),
        ],
    }
}

fn recorded() -> Change {
    Change::Unsnooze {
        messages: ["m-a1", "m-a2", "m-del"]
            .iter()
            .map(|id| (*id).to_string())
            .collect(),
    }
}

/// Inbox, Sent Items, Archive and Deleted Items, all well-known.
async fn mailbox(g: &Graph) {
    g.mount_root(json!([
        folder("f-inbox", "Inbox", 0),
        folder("f-sent", "Sent Items", 0),
        folder("f-arch", "Archive", 0),
        folder("f-del", "Deleted Items", 0),
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

#[tokio::test]
async fn unsnooze_moves_only_the_recorded_messages_still_in_archive_to_the_inbox() {
    let g = Graph::start().await;
    mailbox(&g).await;
    for id in ["m-a1", "m-a2"] {
        let at = format!("/me/messages/{id}/move");
        g.batch.answer("POST", &at, 201, json!({"id": id}));
    }
    g.provider().apply(&[snoozed()], &recorded()).await.unwrap();
    let urls: Vec<String> = g
        .batch
        .requests()
        .iter()
        .map(|r| r["url"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        urls,
        vec!["/me/messages/m-a1/move", "/me/messages/m-a2/move"]
    );
    for request in g.batch.requests() {
        assert_eq!(request["method"], "POST");
        assert_eq!(request["body"], json!({"destinationId": "f-inbox"}));
        assert_eq!(request["headers"]["Prefer"], IMMUTABLE_IDS);
    }
}

#[tokio::test]
async fn unsnooze_counts_a_message_gone_from_the_mailbox_as_done() {
    let g = Graph::start().await;
    mailbox(&g).await;
    g.batch
        .answer("POST", "/me/messages/m-a1/move", 201, json!({"id": "m-a1"}));
    g.batch.answer(
        "POST",
        "/me/messages/m-a2/move",
        404,
        json!({"error": {"code": "ErrorItemNotFound", "message": "gone"}}),
    );
    g.provider().apply(&[snoozed()], &recorded()).await.unwrap();
}
