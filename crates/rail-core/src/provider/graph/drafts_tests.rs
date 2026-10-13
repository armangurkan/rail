//! Outlook drafts through Graph's message model against fake Graph: a new message is posted
//! with Rail's fields; a reply, reply-all and forward are made on their source and patched; an
//! update patches the held draft and a 404 creates it again; a source Outlook lost becomes a
//! new message; a draft lost after its create is found by its Message-ID; a draft outside the
//! Drafts folder (moved to Deleted Items) is gone, so a save makes a new one; ids Rail would
//! not put in a path and messages Rail would not send are refused before any request; every
//! request asks for immutable ids.

use serde_json::{Value, json};
use wiremock::matchers::{body_json, header, method, path, query_param};
use wiremock::{Mock, ResponseTemplate};

use super::fixtures::{Graph, ROOT};
use crate::provider::http::IMMUTABLE_IDS;
use crate::provider::{
    Carried, DraftIds, Mailbox, Outgoing, OutgoingAttachment, OutgoingMail, Provider, ProviderError,
};
use crate::store::drafts::DraftKind;

pub(super) const MESSAGE_ID: &str = "<d1@corp.example>";

pub(super) fn mail(kind: DraftKind, source: Option<&str>, remote: Option<&str>) -> OutgoingMail {
    OutgoingMail {
        outgoing: Outgoing {
            from_name: Some("Me".into()),
            from: "me@corp.example".into(),
            to: vec![Mailbox {
                name: Some("Ann".into()),
                address: "ann@example.com".into(),
            }],
            cc: vec![],
            bcc: vec![Mailbox {
                name: None,
                address: "bo@example.com".into(),
            }],
            subject: "Re: plan".into(),
            message_id: MESSAGE_ID.into(),
            in_reply_to: None,
            references: vec![],
            body: "hi\n".into(),
            attachments: vec![],
        },
        kind,
        source: source.map(str::to_string),
        remote: remote.map(ids),
        thread: Some("c1".into()),
    }
}

/// The draft fields Graph is sent on a create or a `PATCH`.
pub(super) fn fields() -> Value {
    json!({
        "subject": "Re: plan",
        "body": {"contentType": "text", "content": "hi\n"},
        "toRecipients": [{"emailAddress": {"address": "ann@example.com", "name": "Ann"}}],
        "ccRecipients": [],
        "bccRecipients": [{"emailAddress": {"address": "bo@example.com"}}],
        "internetMessageId": MESSAGE_ID
    })
}

pub(super) fn ids(id: &str) -> DraftIds {
    DraftIds {
        draft_id: id.into(),
        message_id: id.into(),
    }
}

pub(super) fn created(id: &str) -> ResponseTemplate {
    ResponseTemplate::new(201).set_body_json(json!({"id": id, "isDraft": true}))
}

/// The `internetMessageId` lookup answers `hits`.
pub(super) async fn found(g: &Graph, hits: Value) {
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages")))
        .and(query_param(
            "$filter",
            format!("internetMessageId eq '{MESSAGE_ID}'"),
        ))
        .and(query_param("$select", "id,isDraft,parentFolderId"))
        .and(header("Prefer", IMMUTABLE_IDS))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": hits})))
        .mount(&g.server)
        .await;
}

/// No message carries Rail's Message-ID yet.
pub(super) async fn none_found(g: &Graph) {
    found(g, json!([])).await;
}

/// Draft `id` holds no attachment: what the reconcile (Task 5) lists after each save.
pub(super) async fn no_attachments(g: &Graph, id: &str) {
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages/{id}/attachments")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": []})))
        .mount(&g.server)
        .await;
}

async fn patched(g: &Graph, id: &str, status: u16) {
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/{id}")))
        .and(body_json(fields()))
        .and(header("Prefer", IMMUTABLE_IDS))
        .respond_with(ResponseTemplate::new(status).set_body_json(json!({"id": id})))
        .expect(1)
        .mount(&g.server)
        .await;
}

async fn posted_new(g: &Graph, id: &str, times: u64) {
    Mock::given(method("POST"))
        .and(path(format!("{ROOT}/me/messages")))
        .and(body_json(fields()))
        .and(header("Prefer", IMMUTABLE_IDS))
        .respond_with(created(id))
        .expect(times)
        .mount(&g.server)
        .await;
}

#[tokio::test]
async fn a_new_draft_is_posted_with_rails_fields_and_its_id_is_both_ids() {
    let g = Graph::start().await;
    none_found(&g).await;
    no_attachments(&g, "g-d1").await;
    posted_new(&g, "g-d1", 1).await;
    let saved = g
        .provider()
        .draft_save(&mail(DraftKind::New, None, None))
        .await;
    assert_eq!(saved.unwrap(), ids("g-d1"));
}

#[tokio::test]
async fn a_reply_reply_all_and_forward_are_made_on_their_source_then_patched() {
    let g = Graph::start().await;
    for (kind, action) in [
        (DraftKind::Reply, "createReply"),
        (DraftKind::ReplyAll, "createReplyAll"),
        (DraftKind::Forward, "createForward"),
    ] {
        g.reset().await;
        none_found(&g).await;
        no_attachments(&g, "g-r1").await;
        Mock::given(method("POST"))
            .and(path(format!("{ROOT}/me/messages/m-src/{action}")))
            .and(body_json(json!({})))
            .and(header("Prefer", IMMUTABLE_IDS))
            .respond_with(created("g-r1"))
            .expect(1)
            .mount(&g.server)
            .await;
        patched(&g, "g-r1", 200).await;
        posted_new(&g, "never", 0).await;
        let saved = g
            .provider()
            .draft_save(&mail(kind, Some("m-src"), None))
            .await;
        assert_eq!(saved.unwrap(), ids("g-r1"), "{action}");
        g.server.verify().await;
    }
}

#[tokio::test]
async fn an_update_patches_the_held_draft_and_a_404_creates_it_again() {
    let g = Graph::start().await;
    no_attachments(&g, "g-d1").await;
    patched(&g, "g-d1", 200).await;
    posted_new(&g, "never", 0).await;
    let saved = g
        .provider()
        .draft_save(&mail(DraftKind::New, None, Some("g-d1")))
        .await;
    assert_eq!(saved.unwrap(), ids("g-d1"));
    let lookups = g
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "GET" && r.url.path() == format!("{ROOT}/me/messages"))
        .count();
    assert_eq!(lookups, 0, "a held id needs no lookup");
    g.server.verify().await;
    g.reset().await;
    no_attachments(&g, "g-d2").await;
    patched(&g, "g-d1", 404).await;
    posted_new(&g, "g-d2", 1).await;
    let saved = g
        .provider()
        .draft_save(&mail(DraftKind::New, None, Some("g-d1")))
        .await;
    assert_eq!(
        saved.unwrap(),
        ids("g-d2"),
        "deleted in Outlook: made again"
    );
}

#[tokio::test]
async fn a_source_outlook_no_longer_has_is_written_as_a_new_message() {
    let g = Graph::start().await;
    none_found(&g).await;
    no_attachments(&g, "g-n1").await;
    Mock::given(method("POST"))
        .and(path(format!("{ROOT}/me/messages/m-gone/createReply")))
        .respond_with(
            ResponseTemplate::new(404)
                .set_body_json(json!({"error": {"code": "ErrorItemNotFound", "message": "gone"}})),
        )
        .expect(1)
        .mount(&g.server)
        .await;
    posted_new(&g, "g-n1", 1).await;
    let saved = g
        .provider()
        .draft_save(&mail(DraftKind::Reply, Some("m-gone"), None))
        .await;
    assert_eq!(saved.unwrap(), ids("g-n1"));
}

#[tokio::test]
async fn a_draft_lost_after_its_create_is_found_by_its_message_id_and_patched() {
    let g = Graph::start().await;
    found(
        &g,
        json!([{"id": "g-sent", "isDraft": false}, {"id": "g-old", "isDraft": true}]),
    )
    .await;
    no_attachments(&g, "g-old").await;
    patched(&g, "g-old", 200).await;
    posted_new(&g, "never", 0).await;
    let saved = g
        .provider()
        .draft_save(&mail(DraftKind::New, None, None))
        .await;
    assert_eq!(saved.unwrap(), ids("g-old"), "the draft, not the sent copy");
}

#[tokio::test]
async fn ids_rail_would_not_put_in_a_path_are_refused_before_their_request() {
    let g = Graph::start().await;
    let provider = g.provider();
    let error = provider
        .draft_save(&mail(DraftKind::New, None, Some("bad/id")))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "{error:?}"
    );
    assert!(g.server.received_requests().await.unwrap().is_empty());
    none_found(&g).await;
    let error = provider
        .draft_save(&mail(DraftKind::Reply, Some("x/../y"), None))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "{error:?}"
    );
    let posts = g.server.received_requests().await.unwrap();
    assert!(
        posts.iter().all(|r| r.method.as_str() == "GET"),
        "only the lookup went"
    );
    g.reset().await;
    found(&g, json!([{"id": "bad id", "isDraft": true}])).await;
    let error = provider
        .draft_save(&mail(DraftKind::New, None, None))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "an unusable id in an answer is an error, never \"not there\": {error:?}"
    );
}

#[tokio::test]
async fn a_message_rail_would_not_send_is_refused_before_any_request() {
    let g = Graph::start().await;
    let provider = g.provider();
    let mut bad = mail(DraftKind::New, None, None);
    bad.outgoing.to[0].address = "ann@example.com\r\nBcc: x@example.com".into();
    let mut many = mail(DraftKind::New, None, None);
    many.outgoing.attachments = (0..51)
        .map(|i| OutgoingAttachment {
            name: format!("{i}.txt"),
            mime: "text/plain".into(),
            bytes: vec![1],
            carried: None,
        })
        .collect();
    let mut large = mail(DraftKind::Forward, Some("m-src"), None);
    large.outgoing.attachments.push(OutgoingAttachment {
        name: "huge.bin".into(),
        mime: "application/octet-stream".into(),
        bytes: vec![],
        carried: Some(Carried {
            message: "m-src".into(),
            part: "p1".into(),
            size: 26 * 1024 * 1024,
        }),
    });
    for refused in [bad, many, large] {
        let error = provider.draft_save(&refused).await.unwrap_err();
        assert!(
            matches!(error, ProviderError::Permanent { .. }),
            "{error:?}"
        );
    }
    assert!(g.server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn a_reply_whose_patch_fails_is_deleted_and_the_patch_error_returned() {
    let g = Graph::start().await;
    none_found(&g).await;
    Mock::given(method("POST"))
        .and(path(format!("{ROOT}/me/messages/m-src/createReply")))
        .respond_with(created("g-r1"))
        .expect(1)
        .mount(&g.server)
        .await;
    patched(&g, "g-r1", 400).await;
    Mock::given(method("DELETE"))
        .and(path(format!("{ROOT}/me/messages/g-r1")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&g.server)
        .await;
    let error = g
        .provider()
        .draft_save(&mail(DraftKind::Reply, Some("m-src"), None))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "{error:?}"
    );
    g.server.verify().await;
}

/// The draft `id`, as a `PATCH` answers it, sits in `folder`.
async fn patched_in(g: &Graph, id: &str, folder: &str) {
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/{id}")))
        .and(body_json(fields()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"id": id, "isDraft": true, "parentFolderId": folder})),
        )
        .expect(1)
        .mount(&g.server)
        .await;
}

/// Requests that would move or delete `id`: none may touch the copy the user put in the trash.
async fn moved_or_deleted(g: &Graph, id: &str) -> usize {
    let at = format!("{ROOT}/me/messages/{id}");
    g.server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| {
            let p = r.url.path();
            (r.method.as_str() == "DELETE" && p == at) || p == format!("{at}/move")
        })
        .count()
}

#[tokio::test]
async fn a_saved_draft_moved_to_deleted_items_is_made_again_in_drafts() {
    let g = Graph::start().await;
    g.mount_well_known(&[("drafts", "f-drafts")]).await;
    none_found(&g).await;
    no_attachments(&g, "g-d1").await;
    posted_new(&g, "g-d1", 1).await;
    let provider = g.provider();
    let first = provider.draft_save(&mail(DraftKind::New, None, None)).await;
    assert_eq!(first.unwrap(), ids("g-d1"));
    g.reset().await;
    g.mount_well_known(&[("drafts", "f-drafts")]).await;
    patched_in(&g, "g-d1", "f-deleted").await;
    no_attachments(&g, "g-d2").await;
    posted_new(&g, "g-d2", 1).await;
    let again = provider
        .draft_save(&mail(DraftKind::New, None, Some("g-d1")))
        .await;
    assert_eq!(again.unwrap(), ids("g-d2"), "a new draft, in Drafts");
    g.server.verify().await;
    assert_eq!(
        moved_or_deleted(&g, "g-d1").await,
        0,
        "the trashed copy stays"
    );
    g.reset().await;
    g.mount_well_known(&[("drafts", "f-drafts")]).await;
    patched_in(&g, "g-d2", "f-drafts").await;
    no_attachments(&g, "g-d2").await;
    posted_new(&g, "never", 0).await;
    let kept = provider
        .draft_save(&mail(DraftKind::New, None, Some("g-d2")))
        .await;
    assert_eq!(kept.unwrap(), ids("g-d2"), "one in Drafts is updated");
}

#[tokio::test]
async fn the_message_id_lookup_takes_only_a_draft_in_drafts() {
    let g = Graph::start().await;
    g.mount_well_known(&[("drafts", "f-drafts")]).await;
    found(
        &g,
        json!([
            {"id": "g-trash", "isDraft": true, "parentFolderId": "f-deleted"},
            {"id": "g-live", "isDraft": true, "parentFolderId": "f-drafts"}
        ]),
    )
    .await;
    no_attachments(&g, "g-live").await;
    patched_in(&g, "g-live", "f-drafts").await;
    posted_new(&g, "never", 0).await;
    let provider = g.provider();
    let saved = provider.draft_save(&mail(DraftKind::New, None, None)).await;
    assert_eq!(
        saved.unwrap(),
        ids("g-live"),
        "the one in Drafts, not the trashed copy"
    );
    g.server.verify().await;
    g.reset().await;
    g.mount_well_known(&[("drafts", "f-drafts")]).await;
    found(
        &g,
        json!([{"id": "g-trash", "isDraft": true, "parentFolderId": "f-deleted"}]),
    )
    .await;
    no_attachments(&g, "g-new").await;
    posted_new(&g, "g-new", 1).await;
    let saved = provider.draft_save(&mail(DraftKind::New, None, None)).await;
    assert_eq!(
        saved.unwrap(),
        ids("g-new"),
        "only a trashed copy: made again"
    );
    assert_eq!(moved_or_deleted(&g, "g-trash").await, 0);
}
