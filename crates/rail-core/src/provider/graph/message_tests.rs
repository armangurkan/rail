//! Graph messages as envelopes: every field, `from` falling back to `sender`, a sent time
//! not believed, hostile text, and messages Rail cannot read.

use serde_json::{Value, json};

use super::fixtures::message;
use super::message::envelope;
use super::wire::MessageWire;
use crate::provider::gmail::MAX_SNIPPET_CHARS;
use crate::provider::{Envelope, Mailbox};

const AT: &str = "2026-10-12T08:30:15Z";

fn read(value: Value, folder: Option<&str>) -> Option<Envelope> {
    envelope(
        serde_json::from_value::<MessageWire>(value).unwrap(),
        folder,
    )
}

#[test]
fn a_message_maps_to_its_envelope() {
    let mut m = message("m1", "c1", "f-inbox", AT);
    m["sentDateTime"] = json!("2026-10-12T08:29:00Z");
    m["flag"] = json!({"flagStatus": "flagged"});
    m["isRead"] = json!(true);
    let e = read(m, Some("f-inbox")).unwrap();
    assert_eq!((e.remote_id.as_str(), e.thread_id.as_str()), ("m1", "c1"));
    assert_eq!(e.message_id.as_deref(), Some("m1@example.com"));
    assert_eq!(e.subject, "subject m1");
    let ann = Mailbox {
        name: Some("Ann".into()),
        address: "ann@example.com".into(),
    };
    assert_eq!(e.from, [ann]);
    let me = Mailbox {
        name: Some("Me".into()),
        address: "me@corp.example".into(),
    };
    assert_eq!(e.to, [me]);
    assert_eq!(e.internal_date, 1_791_793_815_000);
    assert_eq!(e.date, 1_791_793_740, "the sent time");
    assert_eq!(e.snippet, "preview m1");
    assert_eq!(e.folders, ["f-inbox"]);
    assert!(e.is_read && e.is_starred && !e.is_draft);
    assert_eq!((e.in_reply_to, e.references.len(), e.size), (None, 0, 0));
}

#[test]
fn from_falls_back_to_sender_and_an_absurd_sent_time_gives_way() {
    let mut m = message("m1", "c1", "f-inbox", AT);
    m["from"] = Value::Null;
    m["sender"] = json!({"emailAddress": {"address": "bot@example.com"}});
    m["sentDateTime"] = json!("1999-01-01T00:00:00Z");
    let e = read(m, None).unwrap();
    let bot = Mailbox {
        name: None,
        address: "bot@example.com".into(),
    };
    assert_eq!(e.from, [bot]);
    assert_eq!(e.date, 1_791_793_815, "the receive time");
    assert!(e.folders.is_empty(), "no folder given, none kept");
}

#[test]
fn hostile_text_is_put_on_one_line_and_cut() {
    let mut m = message("m1", "c1", "f-inbox", AT);
    m["subject"] = json!("a\r\nBcc: evil@example.com");
    m["bodyPreview"] = json!("x".repeat(1_000));
    let e = read(m, Some("f-inbox")).unwrap();
    assert_eq!(e.subject, "a  Bcc: evil@example.com");
    assert_eq!(e.snippet.chars().count(), MAX_SNIPPET_CHARS);
}

#[test]
fn no_conversation_no_date_or_an_unsafe_id_gives_no_envelope() {
    let mut no_thread = message("m1", "c1", "f-inbox", AT);
    no_thread.as_object_mut().unwrap().remove("conversationId");
    let mut no_date = message("m1", "c1", "f-inbox", AT);
    no_date["receivedDateTime"] = json!("yesterday");
    let unsafe_id = message("a/b", "c1", "f-inbox", AT);
    for broken in [no_thread, no_date, unsafe_id] {
        assert_eq!(read(broken, Some("f-inbox")), None);
    }
}
