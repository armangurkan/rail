//! A draft's attachments made Rail's: the pure match by cleaned name and size (the one Outlook
//! listed when Rail recorded it), in order, each once; against fake Graph, a save reports the
//! size Outlook lists for each attachment, deletes Outlook's extra file attachment and its
//! inline pictures, uploads Rail's new one and keeps the rest, never touching item attachments;
//! a carried
//! original Outlook holds is neither fetched nor uploaded, and one it lost is fetched from its
//! message and uploaded; a listing with a next page is refused; a delete of an attachment gone
//! is done.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::json;
use wiremock::matchers::{body_json, method, path};
use wiremock::{Mock, ResponseTemplate};

use super::drafts_tests::{ids, mail};
use super::fixtures::{Graph, ROOT};
use super::reconcile::{Changes, FILE_ATTACHMENT, Listed, changes};
use crate::provider::{Carried, OutgoingAttachment, Provider, ProviderError};
use crate::store::drafts::DraftKind;

fn file(name: &str, bytes: &[u8]) -> OutgoingAttachment {
    OutgoingAttachment {
        name: name.into(),
        mime: "text/plain".into(),
        bytes: bytes.to_vec(),
        carried: None,
        listed: None,
    }
}

fn carried(name: &str, size: usize) -> OutgoingAttachment {
    OutgoingAttachment {
        name: name.into(),
        mime: "application/pdf".into(),
        bytes: vec![],
        carried: Some(Carried {
            message: "m-src".into(),
            part: "p1".into(),
            size,
        }),
        listed: None,
    }
}

fn listed(id: &str, name: &str, size: usize) -> Listed {
    Listed {
        id: id.into(),
        name: name.into(),
        size,
    }
}

fn attachment_json(
    id: &str,
    name: &str,
    size: usize,
    kind: &str,
    inline: bool,
) -> serde_json::Value {
    json!({"@odata.type": kind, "id": id, "name": name, "size": size,
           "contentType": "text/plain", "isInline": inline})
}

async fn patched(g: &Graph, id: &str) {
    Mock::given(method("PATCH"))
        .and(path(format!("{ROOT}/me/messages/{id}")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"id": id})))
        .mount(&g.server)
        .await;
}

async fn listing(g: &Graph, id: &str, value: serde_json::Value) {
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages/{id}/attachments")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"value": value})))
        .mount(&g.server)
        .await;
}

#[test]
fn matched_kept_missing_uploaded_and_extra_deleted() {
    let wanted = [file("b.txt", b"0123456789"), file("c.txt", b"hello")];
    let have = [listed("att-a", "a.txt", 3), listed("att-b", "b.txt", 10)];
    assert_eq!(
        changes(&have, &wanted),
        Changes {
            delete: vec!["att-a".into()],
            upload: vec![1],
            matched: vec![Some(1), None],
        }
    );
}

#[test]
fn duplicates_match_once_each_in_order() {
    let wanted = [
        file("x.txt", b"aa"),
        file("x.txt", b"aa"),
        file("x.txt", b"aa"),
    ];
    let have = [listed("att-1", "x.txt", 2), listed("att-2", "x.txt", 2)];
    assert_eq!(
        changes(&have, &wanted),
        Changes {
            delete: vec![],
            upload: vec![2],
            matched: vec![Some(0), Some(1), None],
        },
        "two listed copies match the first two; the third goes up"
    );
    let have = [
        listed("att-1", "x.txt", 2),
        listed("att-2", "x.txt", 2),
        listed("att-3", "x.txt", 2),
    ];
    assert_eq!(
        changes(&have, &wanted[..1]),
        Changes {
            delete: vec!["att-2".into(), "att-3".into()],
            upload: vec![],
            matched: vec![Some(0)],
        }
    );
}

#[test]
fn names_are_matched_cleaned_and_a_carried_one_by_its_listed_size() {
    let wanted = [
        file("dir/r\u{202e}eport.txt", b"abc"),
        carried("orig.pdf", 1234),
    ];
    let have = [
        listed("att-r", "report.txt", 3),
        listed("att-o", "orig.pdf", 1234),
    ];
    assert_eq!(
        changes(&have, &wanted),
        Changes {
            delete: vec![],
            upload: vec![],
            matched: vec![Some(0), Some(1)],
        }
    );
}

#[test]
fn a_recorded_listed_size_is_matched_in_place_of_the_bytes() {
    let mut note = file("note.txt", &[b'n'; 41]);
    let have = [listed("att-n", "note.txt", 239)];
    assert_eq!(
        changes(&have, std::slice::from_ref(&note)),
        Changes {
            delete: vec!["att-n".into()],
            upload: vec![0],
            matched: vec![None],
        },
        "nothing recorded: the bytes do not match Outlook's size"
    );
    note.listed = Some(239);
    assert_eq!(
        changes(&have, &[note]),
        Changes {
            delete: vec![],
            upload: vec![],
            matched: vec![Some(0)],
        }
    );
}

#[tokio::test]
async fn a_save_reports_the_size_outlook_lists_for_each_attachment() {
    let g = Graph::start().await;
    patched(&g, "g-d1").await;
    listing(
        &g,
        "g-d1",
        json!([attachment_json(
            "att-b",
            "b.txt",
            205,
            FILE_ATTACHMENT,
            false
        )]),
    )
    .await;
    Mock::given(method("POST"))
        .and(path(format!("{ROOT}/me/messages/g-d1/attachments")))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "att-c", "size": 203})))
        .expect(1)
        .mount(&g.server)
        .await;
    let mut draft = mail(DraftKind::New, None, Some("g-d1"));
    let mut kept = file("b.txt", b"0123456789");
    kept.listed = Some(205);
    draft.outgoing.attachments = vec![kept, file("c.txt", b"hello")];
    let saved = g.provider().draft_save(&draft).await.unwrap();
    assert_eq!(saved.listed, [Some(205), Some(203)]);
    let listings = g
        .server
        .received_requests()
        .await
        .unwrap()
        .iter()
        .filter(|r| r.method.as_str() == "GET")
        .count();
    assert_eq!(
        listings, 1,
        "the post's answer gives the size: no second listing"
    );
}

#[test]
fn a_listed_attachments_debug_shows_no_name() {
    let shown = format!("{:?}", listed("att-a", "secret-plan.pdf", 3));
    assert!(!shown.contains("secret"), "{shown}");
}

#[tokio::test]
async fn a_save_deletes_outlooks_extra_and_its_inline_pictures_and_uploads_rails_new() {
    let g = Graph::start().await;
    patched(&g, "g-d1").await;
    listing(
        &g,
        "g-d1",
        json!([
            attachment_json("att-a", "a.txt", 3, FILE_ATTACHMENT, false),
            attachment_json("att-b", "b.txt", 10, FILE_ATTACHMENT, false),
            attachment_json("att-i", "logo.png", 99, FILE_ATTACHMENT, true),
            attachment_json(
                "att-x",
                "fwd.eml",
                500,
                "#microsoft.graph.itemAttachment",
                false
            ),
        ]),
    )
    .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{ROOT}/me/messages/g-d1/attachments/att-a")))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&g.server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{ROOT}/me/messages/g-d1/attachments")))
        .and(body_json(json!({
            "@odata.type": FILE_ATTACHMENT,
            "name": "c.txt",
            "contentType": "text/plain",
            "contentBytes": STANDARD.encode(b"hello")
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "att-c"})))
        .expect(1)
        .mount(&g.server)
        .await;
    let mut draft = mail(DraftKind::New, None, Some("g-d1"));
    draft.outgoing.attachments = vec![file("b.txt", b"0123456789"), file("c.txt", b"hello")];
    assert_eq!(
        g.provider().draft_save(&draft).await.unwrap().ids,
        ids("g-d1")
    );
    let deleted: Vec<String> = g
        .server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.method.as_str() == "DELETE")
        .map(|r| r.url.path().to_string())
        .collect();
    assert_eq!(
        deleted,
        [
            format!("{ROOT}/me/messages/g-d1/attachments/att-a"),
            format!("{ROOT}/me/messages/g-d1/attachments/att-i")
        ],
        "the inline picture goes with the text body; the item attachment stays"
    );
}

#[tokio::test]
async fn a_carried_original_outlook_holds_is_neither_fetched_nor_uploaded() {
    let g = Graph::start().await;
    patched(&g, "g-f1").await;
    listing(
        &g,
        "g-f1",
        json!([attachment_json(
            "att-o",
            "orig.pdf",
            1234,
            FILE_ATTACHMENT,
            false
        )]),
    )
    .await;
    let mut draft = mail(DraftKind::Forward, Some("m-src"), Some("g-f1"));
    draft.outgoing.attachments = vec![carried("orig.pdf", 1234)];
    g.provider().draft_save(&draft).await.unwrap();
    let requests = g.server.received_requests().await.unwrap();
    assert!(
        requests.iter().all(|r| !r.url.path().ends_with("/$value")
            && !(r.method.as_str() == "POST" && r.url.path().ends_with("/attachments"))),
        "nothing fetched, nothing uploaded"
    );
}

#[tokio::test]
async fn a_carried_original_outlook_lost_is_fetched_from_its_message_and_uploaded() {
    let g = Graph::start().await;
    patched(&g, "g-f1").await;
    listing(&g, "g-f1", json!([])).await;
    Mock::given(method("GET"))
        .and(path(format!(
            "{ROOT}/me/messages/m-src/attachments/p1/$value"
        )))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(b"%PDF".to_vec()))
        .expect(1)
        .mount(&g.server)
        .await;
    Mock::given(method("POST"))
        .and(path(format!("{ROOT}/me/messages/g-f1/attachments")))
        .and(body_json(json!({
            "@odata.type": FILE_ATTACHMENT,
            "name": "orig.pdf",
            "contentType": "application/pdf",
            "contentBytes": STANDARD.encode(b"%PDF")
        })))
        .respond_with(ResponseTemplate::new(201).set_body_json(json!({"id": "att-n"})))
        .expect(1)
        .mount(&g.server)
        .await;
    let mut draft = mail(DraftKind::Forward, Some("m-src"), Some("g-f1"));
    draft.outgoing.attachments = vec![carried("orig.pdf", 1234)];
    g.provider().draft_save(&draft).await.unwrap();
    g.server.verify().await;
}

#[tokio::test]
async fn a_listing_with_a_next_page_is_refused_and_a_gone_attachment_is_deleted_already() {
    let g = Graph::start().await;
    patched(&g, "g-d1").await;
    Mock::given(method("GET"))
        .and(path(format!("{ROOT}/me/messages/g-d1/attachments")))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"value": [], "@odata.nextLink": format!("{}{ROOT}/links/more", g.server.uri())}),
        ))
        .mount(&g.server)
        .await;
    let error = g
        .provider()
        .draft_save(&mail(DraftKind::New, None, Some("g-d1")))
        .await
        .unwrap_err();
    assert!(
        matches!(error, ProviderError::Permanent { .. }),
        "{error:?}"
    );
    g.reset().await;
    patched(&g, "g-d1").await;
    listing(
        &g,
        "g-d1",
        json!([attachment_json("att-a", "a.txt", 3, FILE_ATTACHMENT, false)]),
    )
    .await;
    Mock::given(method("DELETE"))
        .and(path(format!("{ROOT}/me/messages/g-d1/attachments/att-a")))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&g.server)
        .await;
    g.provider()
        .draft_save(&mail(DraftKind::New, None, Some("g-d1")))
        .await
        .unwrap();
}
